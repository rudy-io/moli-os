//! Named providers and the routes that pick one per task.

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::Config;
use crate::llm::Endpoint;
use crate::tier::{Tier, may_receive};

/// A cloud tier needs the context filter (step 2 of the privacy design): until
/// it exists, the conversation can only go to a provider that declares tier 1,
/// that is, one that is explicitly allowed to see what it sees today.
const CONTEXT_FILTER_READY: bool = false;

/// `[[assistant.provider]]` in moli.toml.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Lowercase letters, digits, `-` and `_`: the name routes and the
    /// dashboard use.
    pub id: String,
    /// OpenAI-compatible API (`http://` for a model at home).
    pub base_url: String,
    pub model: String,
    /// The most sensitive tier this provider may receive: 1 a model at home
    /// (or a cloud explicitly given everything), 2 a cloud that only sees the
    /// house's pseudonymised structure, 3 a cloud that only sees free
    /// content. Absent: it receives nothing and cannot be routed.
    #[serde(default)]
    pub tier: Option<u8>,
    /// `moli-v4`: the model was trained on Moli's house contract (its static
    /// prompt, devices numbered, and its thirteen tools, `tools/model/`), so the
    /// conversation speaks that contract to it, in one round trip. Absent: a
    /// general model.
    #[serde(default)]
    pub contract: Option<String>,
}

/// `[assistant.routes]`: which provider serves which task.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routes {
    #[serde(default)]
    pub chat: Option<String>,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub builder: Option<String>,
    #[serde(default)]
    pub audit: Option<String>,
    /// Who reads receipt photos (a model that sees images); absent: the chat
    /// provider. Only a provider of tier 1 is ever sent one.
    #[serde(default)]
    pub receipts: Option<String>,
}

impl Routes {
    fn is_empty(&self) -> bool {
        self.chat.is_none()
            && self.voice.is_none()
            && self.builder.is_none()
            && self.audit.is_none()
            && self.receipts.is_none()
    }
}

/// What a request is for. Voice, builder and audit are only resolved to the
/// chat provider (or nothing) until steps 3 to 5 route them elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // the other routes are constructed from the later steps (and in tests)
pub enum Route {
    Chat,
    Voice,
    Builder,
    Audit,
    /// Receipt photos (tier 1 data: the provider's tier is checked at the call).
    Receipts,
}

/// A provider as the assistant runs it.
#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub base_url: String,
    pub(crate) endpoint: Endpoint,
    pub model: String,
    pub tier: Option<Tier>,
    /// The house-model contract the provider speaks (`moli-v4`), if any.
    pub contract: Option<String>,
    /// A contract declared that this Moli does not speak (`moli-v3` after an
    /// update): the provider is refused, with no fallback to a general model.
    pub refused_contract: Option<String>,
}

/// The providers of an assistant and who serves what.
#[derive(Clone, Debug)]
pub struct Providers {
    list: Vec<Provider>,
    chat: usize,
    voice: usize,
    builder: usize,
    audit: Option<usize>,
    receipts: usize,
    legacy: bool,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The providers declared in `[[assistant.provider]]`, each checked.
fn declared(config: &Config) -> anyhow::Result<Vec<Provider>> {
    let mut list: Vec<Provider> = Vec::new();
    for p in &config.provider {
        ensure!(
            valid_id(&p.id),
            "provider id {:?}: lowercase letters, digits, - and _ only",
            p.id
        );
        ensure!(
            list.iter().all(|q| q.id != p.id),
            "provider id {:?} is declared twice",
            p.id
        );
        let tier = match p.tier {
            None => None,
            Some(level) => Some(
                Tier::from_level(level)
                    .filter(|t| *t != Tier::Never)
                    .with_context(|| {
                        format!("provider {:?}: tier must be 1, 2 or 3 (got {level})", p.id)
                    })?,
            ),
        };
        // A contract this Moli does not speak is refused, not fatal: Moli starts
        // and says why its assistant is out (`check-config` still fails).
        let known = crate::contract::name()?;
        let refused_contract = p.contract.clone().filter(|c| *c != known);
        let endpoint =
            Endpoint::parse(&p.base_url).with_context(|| format!("provider {:?}", p.id))?;
        list.push(Provider {
            id: p.id.clone(),
            base_url: p.base_url.clone(),
            endpoint,
            model: p.model.clone(),
            tier,
            contract: p.contract.clone().filter(|_| refused_contract.is_none()),
            refused_contract,
        });
    }

    Ok(list)
}

impl Providers {
    /// Validates `[assistant]`: without `provider`, the single implicit
    /// provider of `base_url` and `model` (tier 1: it sees what it always saw).
    pub(crate) fn from_config(config: &Config) -> anyhow::Result<Self> {
        if config.provider.is_empty() {
            ensure!(
                config.routes.is_empty(),
                "[assistant.routes] needs at least one [[assistant.provider]]"
            );
            let endpoint = Endpoint::parse(&config.base_url)?;
            return Ok(Self {
                list: vec![Provider {
                    id: "default".into(),
                    base_url: config.base_url.clone(),
                    endpoint,
                    model: config.model.clone(),
                    tier: Some(Tier::LocalOnly),
                    contract: None,
                    refused_contract: None,
                }],
                chat: 0,
                voice: 0,
                builder: 0,
                audit: None,
                receipts: 0,
                legacy: true,
            });
        }

        let list = declared(config)?;

        let find = |route: &str, id: &Option<String>| -> anyhow::Result<Option<usize>> {
            match id {
                None => Ok(None),
                Some(id) => list
                    .iter()
                    .position(|p| &p.id == id)
                    .map(Some)
                    .with_context(|| format!("routes.{route}: no provider {id:?}")),
            }
        };
        let chat = find("chat", &config.routes.chat)?
            .context("routes.chat is required when providers are declared")?;
        let voice_route = find("voice", &config.routes.voice)?;
        let builder_route = find("builder", &config.routes.builder)?;
        let audit = find("audit", &config.routes.audit)?;
        let receipts_route = find("receipts", &config.routes.receipts)?;
        let voice = voice_route.unwrap_or(chat);
        let builder = builder_route.unwrap_or(chat);
        let receipts = receipts_route.unwrap_or(chat);

        for (route, index) in [
            ("chat", Some(chat)),
            ("voice", Some(voice)),
            ("builder", Some(builder)),
            ("audit", audit),
            ("receipts", Some(receipts)),
        ] {
            let Some(index) = index else { continue };
            ensure!(
                list[index].tier.is_some(),
                "routes.{route}: provider {:?} has no tier, so it receives nothing",
                list[index].id
            );
        }

        if let Some(index) = audit {
            // The words of a question go there: never in clear over the Internet.
            ensure!(
                !list[index].endpoint.is_local() || list[index].endpoint.at_home(),
                "routes.audit: provider {:?} would send over plain http to an address that is not at home: use https",
                list[index].id
            );
        }

        if let Some(index) = receipts_route {
            // The photos of what the household buys: never in clear over the Internet.
            ensure!(
                !list[index].endpoint.is_local() || list[index].endpoint.at_home(),
                "routes.receipts: provider {:?} would send over plain http to an address that is not at home: use https",
                list[index].id
            );
        }

        if !CONTEXT_FILTER_READY {
            ensure!(
                voice == chat,
                "routes.voice must name the chat provider for now (one provider serves the conversation)"
            );
            ensure!(
                builder == chat,
                "routes.builder must name the chat provider for now (one provider serves the conversation)"
            );
            // The conversation carries the house's names and live values (tier 1).
            ensure!(
                list[chat]
                    .tier
                    .is_some_and(|t| may_receive(t, Tier::LocalOnly)),
                "routes.chat: provider {:?} must have tier 1 for now (a cloud tier needs the context filter, not available yet)",
                list[chat].id
            );
        }

        Ok(Self {
            list,
            chat,
            voice,
            builder,
            audit,
            receipts,
            legacy: false,
        })
    }

    /// The provider that serves `route`; `None` for an `audit` route that is
    /// not configured.
    pub(crate) fn resolve(&self, route: Route) -> Option<&Provider> {
        let index = match route {
            Route::Chat => Some(self.chat),
            Route::Voice => Some(self.voice),
            Route::Builder => Some(self.builder),
            Route::Audit => self.audit,
            Route::Receipts => Some(self.receipts),
        };
        index.map(|i| &self.list[i])
    }

    /// Who reads receipts: the `receipts` route, or else the chat provider.
    pub(crate) fn receipts(&self) -> &Provider {
        &self.list[self.receipts]
    }

    /// The secret (of the `assistant` instance) that holds the API key of the
    /// provider serving `route`: the historical `api_key` for an old config,
    /// `api_key.<id>` for a named provider, so that a key given to one host is
    /// never sent to another.
    pub(crate) fn key_name(&self, route: Route) -> Option<String> {
        let provider = self.resolve(route)?;
        Some(if self.legacy {
            "api_key".to_owned()
        } else {
            format!("api_key.{}", provider.id)
        })
    }

    /// The chat provider's contract, when this Moli refuses it: (provider,
    /// contract declared, contract spoken).
    pub(crate) fn refused_chat(&self) -> Option<(&str, &str, String)> {
        let chat = &self.list[self.chat];
        let declared = chat.refused_contract.as_deref()?;
        Some((
            &chat.id,
            declared,
            crate::contract::name().unwrap_or_default(),
        ))
    }

    /// Whether a provider speaks the house-model contract.
    pub(crate) fn any_contract(&self) -> bool {
        self.list.iter().any(|p| p.contract.is_some())
    }

    pub(crate) fn is_legacy(&self) -> bool {
        self.legacy
    }

    /// What the dashboard shows of the privacy setup.
    pub(crate) fn summary(&self) -> Json {
        let id = |i: usize| self.list[i].id.clone();
        json!({
            "mode": if self.legacy { "legacy" } else { "named" },
            "providers": self.list.iter().map(|p| json!({
                "id": p.id,
                "tier": p.tier.map(Tier::level),
                "local": p.endpoint.at_home(),
                "contract": p.contract,
                "refused_contract": p.refused_contract,
            })).collect::<Vec<_>>(),
            "routes": {
                "chat": id(self.chat),
                "voice": id(self.voice),
                "builder": id(self.builder),
                "audit": self.audit.map(id),
                "receipts": id(self.receipts),
            },
        })
    }

    /// Said once at start: the house's names and live values go to a cloud
    /// provider: an old `[assistant]` with a key filed, or without TLS (then the
    /// data goes in clear whether or not there is a key), to an address that is
    /// not at home.
    pub(crate) fn warning(&self, key_present: bool) -> Option<String> {
        let chat = &self.list[self.chat];
        if chat.endpoint.at_home() {
            return None;
        }
        if self.legacy {
            return (key_present || chat.endpoint.is_local()).then(|| {
                format!(
                    "assistant: the house's names and live values (tier 1) go to {}; declare [[assistant.provider]] to choose where each request may go",
                    chat.base_url
                )
            });
        }
        // A named provider out of the house was chosen on purpose: said anyway, so that it shows in the logs.
        Some(format!(
            "assistant: provider {:?} (tier 1) receives the house's names and live values at {}, outside the house",
            chat.id, chat.base_url
        ))
    }
}

impl Config {
    /// Validates `[assistant]` (what `moli-os check-config` runs).
    pub fn check(&self) -> anyhow::Result<()> {
        let (_, providers) = self.clone().resolved()?;
        for p in &providers.list {
            if let Some(declared) = &p.refused_contract {
                anyhow::bail!(
                    "provider {:?}: contract {declared:?} is not the one this Moli speaks ({})",
                    p.id,
                    crate::contract::name()?
                );
            }
        }
        Ok(())
    }

    /// The configuration the rest of the assistant runs on, and the providers.
    /// With named providers, the chat provider's address and model replace
    /// `base_url`, `model` and `builder_model` (one provider serves the
    /// conversation for now), and the web search, which goes through a cloud
    /// provider's own search, is off.
    pub(crate) fn resolved(self) -> anyhow::Result<(Self, Providers)> {
        let providers = Providers::from_config(&self)?;
        let mut effective = self;
        if !providers.is_legacy() {
            let chat = providers
                .resolve(Route::Chat)
                .context("the chat route was validated")?;
            effective.base_url.clone_from(&chat.base_url);
            effective.model.clone_from(&chat.model);
            effective.builder_model.clone_from(&chat.model);
            effective.search_model = String::new();
            // The cloud voice would go to the chat provider: a model at home has
            // none (Ollama answers neither), so the local voices (Whisper, Piper)
            // serve, and the status does not claim a cloud voice. Only the defaults
            // go: a voice model set by hand is the owner's choice.
            if chat.endpoint.at_home() {
                if effective.transcribe_model == crate::default_transcribe() {
                    effective.transcribe_model = String::new();
                }
                if effective.speech_model == crate::default_speech_model() {
                    effective.speech_model = String::new();
                }
            }
        }
        Ok((effective, providers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    fn err(text: &str) -> String {
        format!("{:#}", cfg(text).resolved().unwrap_err())
    }

    #[test]
    fn a_contract_this_moli_does_not_speak_is_refused_not_fatal() {
        let old = LOCAL.replace("tier = 1\n", "tier = 1\ncontract = \"moli-v1\"\n");
        // Moli still starts: the provider is refused, and says why.
        let (_, providers) = cfg(&old).resolved().unwrap();
        let (id, declared, known) = providers.refused_chat().unwrap();
        assert_eq!((id, declared), ("local", "moli-v1"));
        assert_eq!(known, crate::contract::name().unwrap());
        assert!(
            !providers.any_contract(),
            "no fallback: nobody speaks the contract"
        );
        // `moli-os check-config` names it.
        let e = format!("{:#}", cfg(&old).check().unwrap_err());
        assert!(e.contains("moli-v1") && e.contains("local"), "{e}");
        // The contract this Moli speaks is accepted.
        let now = LOCAL.replace("tier = 1\n", &format!("tier = 1\ncontract = \"{known}\"\n"));
        assert!(cfg(&now).check().is_ok());
        assert!(cfg(&now).resolved().unwrap().1.refused_chat().is_none());
    }

    const LOCAL: &str = r#"
[[provider]]
id = "local"
base_url = "http://127.0.0.1:11434/v1"
model = "small-model"
tier = 1

[routes]
chat = "local"
"#;

    #[test]
    fn an_existing_config_is_one_implicit_provider_at_tier_one() {
        let (effective, providers) = cfg("").resolved().unwrap();
        assert!(providers.is_legacy());
        let chat = providers.resolve(Route::Chat).unwrap();
        assert_eq!(chat.id, "default");
        assert_eq!(chat.tier, Some(Tier::LocalOnly));
        assert_eq!(chat.model, "gpt-4.1-mini");
        assert!(providers.resolve(Route::Audit).is_none());
        // Nothing is replaced: the rest of the assistant runs as before.
        assert_eq!(effective.base_url, "https://api.openai.com/v1");
        assert_eq!(effective.model, "gpt-4.1-mini");
        assert_eq!(effective.search_model, "gpt-4.1-mini");
    }

    #[test]
    fn routes_without_providers_are_an_error_not_a_silence() {
        let e = err("[routes]\nchat = \"local\"\n");
        assert!(
            e.contains("needs at least one [[assistant.provider]]"),
            "{e}"
        );
    }

    #[test]
    fn a_local_provider_serves_the_conversation_voice_and_builder() {
        let (effective, providers) = cfg(LOCAL).resolved().unwrap();
        assert!(!providers.is_legacy());
        for route in [Route::Chat, Route::Voice, Route::Builder] {
            assert_eq!(providers.resolve(route).unwrap().id, "local");
        }
        assert!(providers.resolve(Route::Audit).is_none());
        // The chat provider replaces the endpoint and the models; the web
        // search (a cloud provider's own) is off in this mode.
        assert_eq!(effective.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(effective.model, "small-model");
        assert_eq!(effective.builder_model, "small-model");
        assert_eq!(effective.search_model, "");
    }

    #[test]
    fn the_chat_route_is_required_with_providers() {
        let e = err(&LOCAL.replace("[routes]\nchat = \"local\"\n", ""));
        assert!(e.contains("routes.chat is required"), "{e}");
    }

    #[test]
    fn a_route_to_an_unknown_provider_names_the_route() {
        let e = err(&LOCAL.replace("chat = \"local\"", "chat = \"nope\""));
        assert!(e.contains("routes.chat") && e.contains("\"nope\""), "{e}");
    }

    #[test]
    fn a_provider_without_a_tier_cannot_be_routed() {
        let e = err(&LOCAL.replace("tier = 1\n", ""));
        assert!(e.contains("has no tier"), "{e}");
    }

    #[test]
    fn tier_zero_and_beyond_three_are_refused() {
        for bad in ["0", "4", "9"] {
            let e = err(&LOCAL.replace("tier = 1", &format!("tier = {bad}")));
            assert!(e.contains("tier must be 1, 2 or 3"), "{bad}: {e}");
        }
    }

    #[test]
    fn provider_ids_are_lowercase_words_and_unique() {
        let e = err(&LOCAL.replace("id = \"local\"", "id = \"Local Host\""));
        assert!(e.contains("lowercase letters"), "{e}");
        let twice = format!(
            "{LOCAL}\n[[provider]]\nid = \"local\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"m\"\ntier = 1\n"
        );
        let e = err(&twice);
        assert!(e.contains("declared twice"), "{e}");
    }

    #[test]
    fn a_cloud_tier_is_refused_for_the_conversation_until_the_context_filter_exists() {
        let e = err(&LOCAL.replace("tier = 1", "tier = 2"));
        assert!(e.contains("must have tier 1 for now"), "{e}");
    }

    #[test]
    fn the_builder_and_the_voice_cannot_leave_the_chat_provider_yet() {
        let other = format!(
            "{LOCAL}\n[[provider]]\nid = \"other\"\nbase_url = \"http://127.0.0.1:2/v1\"\nmodel = \"m\"\ntier = 1\n"
        );
        for route in ["builder", "voice"] {
            let e = err(&other.replace("[routes]\n", &format!("[routes]\n{route} = \"other\"\n")));
            assert!(e.contains("must name the chat provider"), "{route}: {e}");
        }
    }

    #[test]
    fn the_audit_route_never_sends_over_plain_http_to_an_address_that_is_not_at_home() {
        let text = format!(
            "{}\n[[provider]]\nid = \"far\"\nbase_url = \"http://api.example.org/v1\"\nmodel = \"m\"\ntier = 3\n",
            LOCAL.replace("[routes]\n", "[routes]\naudit = \"far\"\n")
        );
        let e = err(&text);
        assert!(e.contains("plain http") && e.contains("audit"), "{e}");
        // At home, plain http is fine (a bigger model on the LAN).
        let home = text.replace("http://api.example.org/v1", "http://10.0.0.2:11434/v1");
        assert!(cfg(&home).resolved().is_ok());
    }

    #[test]
    fn the_audit_route_may_use_a_cloud_provider_of_tier_two() {
        let text = format!(
            "{}\n[[provider]]\nid = \"mistral\"\nbase_url = \"https://api.example.org/v1\"\nmodel = \"m\"\ntier = 2\n",
            LOCAL.replace("[routes]\n", "[routes]\naudit = \"mistral\"\n")
        );
        let (_, providers) = cfg(&text).resolved().unwrap();
        let audit = providers.resolve(Route::Audit).unwrap();
        assert_eq!(audit.id, "mistral");
        assert_eq!(audit.tier, Some(Tier::Pseudonymised));
    }

    #[test]
    fn the_warning_only_fires_when_house_data_really_goes_to_a_cloud() {
        let (_, legacy) = cfg("").resolved().unwrap();
        assert!(legacy.warning(false).is_none(), "no key, nothing is sent");
        assert!(legacy.warning(true).unwrap().contains("api.openai.com"));
        let (_, local_legacy) = cfg("base_url = \"http://127.0.0.1:11434/v1\"")
            .resolved()
            .unwrap();
        assert!(local_legacy.warning(true).is_none(), "a model at home");
        let (_, named) = cfg(LOCAL).resolved().unwrap();
        assert!(named.warning(true).is_none());
        // A named tier-1 provider in a cloud is allowed (a choice), and said at start.
        let cloud = LOCAL.replace("http://127.0.0.1:11434/v1", "https://api.example.com/v1");
        let (_, named_cloud) = cfg(&cloud).resolved().unwrap();
        let said = named_cloud.warning(false).unwrap();
        assert!(
            said.contains("\"local\"") && said.contains("api.example.com"),
            "{said}"
        );
    }

    #[test]
    fn a_provider_at_home_gets_no_cloud_voice_a_cloud_one_keeps_it() {
        let (home, _) = cfg(LOCAL).resolved().unwrap();
        assert!(home.transcribe_model.is_empty() && home.speech_model.is_empty());
        let cloud = LOCAL.replace("http://127.0.0.1:11434/v1", "https://api.example.com/v1");
        let (away, _) = cfg(&cloud).resolved().unwrap();
        let defaults = cfg("");
        assert_eq!(away.transcribe_model, defaults.transcribe_model);
        assert_eq!(away.speech_model, defaults.speech_model);
        // A voice model set by hand stays.
        let chosen = LOCAL.replace("[[provider]]", "speech_model = \"ma-voix\"\n\n[[provider]]");
        let (mine, _) = cfg(&chosen).resolved().unwrap();
        assert_eq!(mine.speech_model, "ma-voix");
        assert!(mine.transcribe_model.is_empty());
        // The old single provider is left as it was.
        let (legacy, _) = cfg("base_url = \"http://127.0.0.1:11434/v1\"")
            .resolved()
            .unwrap();
        assert_eq!(legacy.speech_model, defaults.speech_model);
    }

    #[test]
    fn the_default_config_of_a_house_without_assistant_section_behaves_as_before() {
        // `start_assistant` uses `Config::default()`, not the serde defaults.
        let (effective, providers) = Config::default().resolved().unwrap();
        assert!(providers.is_legacy());
        assert_eq!(effective.base_url, "https://api.openai.com/v1");
        assert_eq!(effective.model, "gpt-4.1-mini");
        assert_eq!(effective.search_model, "gpt-4.1-mini");
        assert!(
            providers.warning(false).is_none(),
            "no key, nothing is sent"
        );
    }

    #[test]
    fn a_tier_that_does_not_fit_a_byte_is_refused_at_loading() {
        for bad in ["256", "300", "-1"] {
            let text = LOCAL.replace("tier = 1", &format!("tier = {bad}"));
            assert!(toml::from_str::<Config>(&text).is_err(), "tier = {bad}");
        }
    }

    #[test]
    fn a_base_url_without_a_scheme_names_the_provider() {
        let e = err(&LOCAL.replace("http://127.0.0.1:11434/v1", "127.0.0.1:11434/v1"));
        assert!(
            e.contains("provider \"local\"") && e.contains("must start with"),
            "{e}"
        );
    }

    #[test]
    fn plain_http_to_a_public_host_is_not_at_home() {
        let remote = "base_url = \"http://api.example.com/v1\"";
        let (_, legacy) = cfg(remote).resolved().unwrap();
        assert!(legacy.warning(true).unwrap().contains("api.example.com"));
        // Without TLS the data goes in clear whether or not a key is filed.
        assert!(legacy.warning(false).unwrap().contains("api.example.com"));
        assert_eq!(legacy.summary()["providers"][0]["local"], false);
    }

    #[test]
    fn the_key_belongs_to_its_provider() {
        let (_, legacy) = cfg("").resolved().unwrap();
        assert_eq!(legacy.key_name(Route::Chat).as_deref(), Some("api_key"));
        let (_, named) = cfg(LOCAL).resolved().unwrap();
        assert_eq!(
            named.key_name(Route::Chat).as_deref(),
            Some("api_key.local")
        );
        assert_eq!(named.key_name(Route::Audit), None);
    }

    #[test]
    fn receipts_go_to_their_route_or_else_to_the_chat_provider() {
        let (_, legacy) = cfg("").resolved().unwrap();
        assert_eq!(legacy.resolve(Route::Receipts).unwrap().id, "default");
        assert_eq!(legacy.key_name(Route::Receipts).as_deref(), Some("api_key"));
        let (_, named) = cfg(LOCAL).resolved().unwrap();
        assert_eq!(named.resolve(Route::Receipts).unwrap().id, "local");
        // A route of their own, with its own key; its tier is checked at the
        // call (the settings page says why a tier 3 cannot read them).
        let text = format!(
            "{}\n[[provider]]\nid = \"eyes\"\nbase_url = \"https://api.example.org/v1\"\nmodel = \"vision\"\ntier = 3\n",
            LOCAL.replace("[routes]\n", "[routes]\nreceipts = \"eyes\"\n")
        );
        let (_, routed) = cfg(&text).resolved().unwrap();
        assert_eq!(routed.resolve(Route::Receipts).unwrap().id, "eyes");
        assert_eq!(
            routed.key_name(Route::Receipts).as_deref(),
            Some("api_key.eyes")
        );
        assert_eq!(routed.summary()["routes"]["receipts"], "eyes");
        // Never over plain http to an address that is not at home.
        let e = err(&text.replace("https://api.example.org", "http://api.example.org"));
        assert!(e.contains("plain http") && e.contains("receipts"), "{e}");
        let e = err(&text.replace("tier = 3\n", ""));
        assert!(
            e.contains("routes.receipts") && e.contains("no tier"),
            "{e}"
        );
    }

    #[test]
    fn the_summary_tells_the_dashboard_what_is_active() {
        let (_, providers) = cfg(LOCAL).resolved().unwrap();
        let s = providers.summary();
        assert_eq!(s["mode"], "named");
        assert_eq!(s["routes"]["chat"], "local");
        assert_eq!(s["providers"][0]["tier"], 1);
        assert_eq!(s["providers"][0]["local"], true);
    }
}
