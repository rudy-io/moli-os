//! Reading a receipt: its photos go to a model that sees images, and come
//! back as a reading shaped like the pantry's receipt draft, for a person to
//! correct and validate. A receipt says where and when the household shops
//! (tier 1): only a provider allowed to see the house's own data reads one.
//!
//! The reading is built here without knowing the pantry: its JSON has the
//! fields of `moli_pantry::ReceiptDraft` (all but `items_done`), and the
//! caller turns it into one.

use std::fmt::Write as _;
use std::time::Instant;

use moli_core::InstanceId;
use serde::Serialize;
use serde_json::{Map, Value as Json, json};

use crate::exchanges::{Cost, Exchange, Kind};
use crate::providers::{Provider, Route};
use crate::tier::{Tier, may_receive};
use crate::{Assistant, AssistantError, is_reasoning, pricing, takes_temperature};

/// Photos of one receipt (a long one is photographed in parts).
pub const MAX_PHOTOS: usize = 4;
/// A run of digits this long is a card, authorisation or transaction number,
/// never a product's: it is not kept, whatever the model copied.
const LONG_NUMBER: usize = 6;
/// What the model may write: a long receipt is some sixty lines of a dozen
/// fields each.
const WORDS: u32 = 8_000;
/// A reasoning model thinks within the same budget: twice as much.
const THINKING_WORDS: u32 = 16_000;

/// What the model is told, in English (its answer keeps the receipt's words).
const RULES: &str = "\
You read the photos of one shop receipt (several photos are parts of the same receipt, in order) and fill in the JSON schema. Keep labels exactly as printed.
Rules:
- One line per article. \"2 x 3,99\" means qty 2 and price_cents 399. line_cents is what the line cost, discount deducted.
- A weighed product: qty in kg, price_cents per kg, unit \"kg\".
- A discount printed under a line goes in that line's discount_cents (a positive number), never in a line of its own.
- keep is true for what the household consumes and buys again (food, drinks, hygiene, cleaning, baby, pets), false for what lasts (decoration, furniture, toys, clothes, DIY, electronics) and for bags or deposits. reason says why in a few words, and any doubt about the reading.
- product: only one of the product ids given, when the line is clearly that product; otherwise null, and new_product a readable name.
- provider: the id of a known chain when the receipt is theirs, otherwise the chain's name as printed. store: the town of the shop.
- at_local: the date and time printed, local time, \"YYYY-MM-DD HH:MM\", or null.
- total_cents: the total printed, or null. vat: the VAT rate in percent, or null. aisle: the product's aisle, or null.
- Never copy a card number, an authorisation number or a transaction number, anywhere.
- If the photos are not a readable receipt: unreadable true and no lines.";

/// Who reads receipts, as the settings page shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReceiptReader {
    pub provider: String,
    pub model: String,
    /// The provider may see receipts (tier 1) and has its key.
    pub allowed: bool,
    /// Why not, in the house's language.
    pub reason: Option<String>,
}

/// The receipts route, resolved: the provider, its model, its key.
struct ReceiptRoute<'a> {
    provider: &'a Provider,
    model: String,
    key: Option<String>,
    refused: Option<String>,
}

impl Assistant {
    /// Who reads receipts, and whether they may.
    pub fn receipt_reader(&self) -> ReceiptReader {
        let route = self.receipt_route();
        ReceiptReader {
            provider: route.provider.id.clone(),
            model: route.model,
            allowed: route.refused.is_none(),
            reason: route.refused,
        }
    }

    fn receipt_route(&self) -> ReceiptRoute<'_> {
        let providers = &self.0.providers;
        let provider = providers.receipts();
        let wanted = self.0.config.receipts_model.trim();
        let model = if providers.is_legacy() && !wanted.is_empty() {
            wanted.to_owned()
        } else {
            provider.model.clone()
        };
        let name = providers
            .key_name(Route::Receipts)
            .unwrap_or_else(|| "api_key".to_owned());
        let key = self.0.hub.secret(&InstanceId::from("assistant"), &name);
        // Photos cannot be pseudonymised: tier 1, whatever the context filter.
        let refused = if !provider
            .tier
            .is_some_and(|t| may_receive(t, Tier::LocalOnly))
        {
            Some(moli_i18n::tr!(
                "assistant.receipt.tier",
                provider = provider.id
            ))
        } else if key.is_none() && !provider.endpoint.is_local() {
            Some(moli_i18n::tr!("assistant.receipt.no_key", name = name))
        } else {
            None
        };
        ReceiptRoute {
            provider,
            model,
            key,
            refused,
        }
    }

    /// Reads the photos of one receipt, in order. `products` are the
    /// pantry's (id, name, aisle), `known_providers` its chains (id, name).
    /// Gives the reading (the fields of the pantry's receipt draft) and who
    /// read it (`provider/model`).
    pub async fn read_receipt(
        &self,
        photos: &[Vec<u8>],
        products: &[(String, String, Option<String>)],
        known_providers: &[(String, String)],
    ) -> Result<(Json, String), AssistantError> {
        if photos.is_empty() || photos.len() > MAX_PHOTOS || photos.iter().any(Vec::is_empty) {
            return Err(AssistantError::Invalid(format!(
                "1 to {MAX_PHOTOS} photos of one receipt"
            )));
        }
        let route = self.receipt_route();
        if let Some(why) = route.refused {
            return Err(AssistantError::NotConfigured(why));
        }
        let _permit = self.admit()?;
        let started = Instant::now();
        let now = jiff::Timestamp::now().to_zoned(self.0.tz.clone());
        let body = request(
            &route.model,
            &crate::house_date(&now),
            photos,
            products,
            known_providers,
        );
        let answer = route
            .provider
            .endpoint
            .chat(route.key.as_deref(), &body)
            .await
            .map_err(|e| AssistantError::Upstream(e.to_string()))?;
        let think = pricing::chat(&route.model, &answer["usage"]);
        let content = answer["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        let reading = tidy(content, products, known_providers, &self.0.tz);
        // A reading that failed is still a receipt (paid for, not a conversation).
        let reply = match &reading {
            Ok(read) => moli_i18n::tr!(
                "assistant.receipt.lines",
                count = read["lines"].as_array().map_or(0, Vec::len)
            ),
            Err(_) => moli_i18n::tr!("assistant.receipt.failed"),
        };
        self.0.exchanges.add(Exchange {
            at: jiff::Timestamp::now().as_millisecond(),
            kind: Kind::Receipt,
            surface: "cellier".into(),
            spoken: false,
            question: moli_i18n::tr!("assistant.receipt.question"),
            reply,
            tools: Vec::new(),
            orders: Vec::new(),
            ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            audio: None,
            cost: Cost::new(think, 0.0, 0.0, 0.0),
        });
        let read = reading.map_err(AssistantError::Upstream)?;
        tracing::info!(
            lines = read["lines"].as_array().map_or(0, Vec::len),
            "receipt read"
        );
        Ok((read, format!("{}/{}", route.provider.id, route.model)))
    }
}

/// The chat completion that reads the photos.
fn request(
    model: &str,
    today: &str,
    photos: &[Vec<u8>],
    products: &[(String, String, Option<String>)],
    known_providers: &[(String, String)],
) -> Json {
    let mut text = format!("Today: {today}.\nKnown chains (id: name):\n");
    for (id, name) in known_providers {
        let _ = writeln!(text, "- {id}: {name}");
    }
    text.push_str("Products of the pantry (id: name, aisle):\n");
    for (id, name, aisle) in products {
        match aisle {
            Some(aisle) => writeln!(text, "- {id}: {name}, {aisle}"),
            None => writeln!(text, "- {id}: {name}"),
        }
        .unwrap_or_default();
    }
    let mut content = vec![json!({ "type": "text", "text": text })];
    for photo in photos {
        content.push(json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", mime(photo), base64(photo)),
                "detail": "high",
            },
        }));
    }
    let mut body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": RULES },
            { "role": "user", "content": content },
        ],
        "response_format": { "type": "json_schema", "json_schema": { "name": "receipt", "strict": true, "schema": schema() } },
    });
    // As the automation builder, with room for a long receipt.
    if is_reasoning(model) {
        body["max_completion_tokens"] = json!(THINKING_WORDS);
        body["reasoning_effort"] = json!("low");
    } else {
        if takes_temperature(model) {
            body["temperature"] = json!(0.2);
        }
        body["max_tokens"] = json!(WORDS);
    }
    body
}

/// An object of the strict schema: every property required, nothing else.
fn object(fields: &[(&str, Json)]) -> Json {
    let properties: Map<String, Json> = fields
        .iter()
        .map(|(name, schema)| ((*name).to_owned(), schema.clone()))
        .collect();
    let required: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// The fields of the pantry's `ReceiptDraft` and `DraftLine` (without
/// `items_done`, a person's choice), `at` asked as local time.
pub(crate) fn schema() -> Json {
    let text = || json!({ "type": "string" });
    let maybe = |kind: &str| json!({ "type": [kind, "null"] });
    let line = object(&[
        ("label", text()),
        ("name", text()),
        ("qty", json!({ "type": "number" })),
        ("unit", maybe("string")),
        ("price_cents", json!({ "type": "integer" })),
        ("discount_cents", json!({ "type": "integer" })),
        ("line_cents", json!({ "type": "integer" })),
        ("vat", maybe("number")),
        ("aisle", maybe("string")),
        ("keep", json!({ "type": "boolean" })),
        ("reason", text()),
        ("product", maybe("string")),
        ("new_product", maybe("string")),
    ]);
    object(&[
        ("provider", text()),
        ("store", text()),
        ("at_local", maybe("string")),
        ("total_cents", maybe("integer")),
        ("lines", json!({ "type": "array", "items": line })),
        ("unreadable", json!({ "type": "boolean" })),
    ])
}

/// The model's answer as a reading: `at_local` becomes `at` (ms, the
/// house's zone), a known chain's name its id, a product id the pantry does
/// not have a product to create, and long numbers go.
fn tidy(
    content: &str,
    products: &[(String, String, Option<String>)],
    known_providers: &[(String, String)],
    tz: &jiff::tz::TimeZone,
) -> Result<Json, String> {
    let text = content.trim();
    // Some local models fence their JSON.
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .map_or(text, |t| t.strip_suffix("```").unwrap_or(t))
        .trim();
    let Ok(Json::Object(mut read)) = serde_json::from_str::<Json>(text) else {
        return Err("the reading is not a JSON object".into());
    };
    if !read.get("lines").is_some_and(Json::is_array) {
        return Err("the reading has no lines".into());
    }
    // A model that does not hold to the schema (some local ones) may leave
    // fields out: the draft needs them.
    fill(&mut read, "provider", json!(""));
    fill(&mut read, "store", json!(""));
    fill(&mut read, "total_cents", Json::Null);
    fill(&mut read, "unreadable", json!(false));
    whole_cents(&mut read);
    let at = read
        .remove("at_local")
        .as_ref()
        .and_then(Json::as_str)
        .and_then(|local| local_ms(local, tz));
    read.insert("at".into(), json!(at));
    if let Some(printed) = read.get("provider").and_then(Json::as_str) {
        let printed = printed.trim();
        let wanted = printed.to_lowercase();
        let provider = known_providers
            .iter()
            .find(|(id, name)| id.to_lowercase() == wanted || name.trim().to_lowercase() == wanted)
            .map_or_else(|| scrub(printed), |(id, _)| id.clone());
        read.insert("provider".into(), json!(provider));
    }
    if let Some(Json::String(store)) = read.get_mut("store") {
        *store = scrub(store);
    }
    let unreadable = read.get("unreadable") == Some(&Json::Bool(true));
    if let Some(Json::Array(lines)) = read.get_mut("lines") {
        if unreadable {
            lines.clear();
        }
        for line in lines.iter_mut().filter_map(Json::as_object_mut) {
            fill(line, "keep", json!(true));
            fill(line, "reason", json!(""));
            fill(line, "discount_cents", json!(0));
            fill(line, "label", json!(""));
            fill(line, "name", json!(""));
            whole_cents(line);
            for field in ["label", "name", "unit", "aisle", "reason", "new_product"] {
                if let Some(Json::String(words)) = line.get_mut(field) {
                    *words = scrub(words);
                }
            }
            let Some(id) = line.get("product").and_then(Json::as_str) else {
                continue;
            };
            if products.iter().any(|(known, _, _)| known == id) {
                // An existing product is never created again.
                line.insert("new_product".into(), Json::Null);
            } else {
                line.insert("product".into(), Json::Null);
                if line
                    .get("new_product")
                    .and_then(Json::as_str)
                    .is_none_or(|n| n.trim().is_empty())
                {
                    let name = line.get("name").cloned().unwrap_or(Json::Null);
                    line.insert("new_product".into(), name);
                }
            }
        }
    }
    Ok(Json::Object(read))
}

/// A model may write an amount as `99.0`: an amount of cents is a whole
/// number, and the draft refuses anything else. Every `*_cents` field that
/// holds a whole float becomes an integer.
fn whole_cents(object: &mut Map<String, Json>) {
    for (key, value) in object.iter_mut() {
        if !key.ends_with("_cents") || !value.is_f64() {
            continue;
        }
        let Some(cents) = value.as_f64() else {
            continue;
        };
        if cents.fract() == 0.0 && cents.abs() < 9e15 {
            #[allow(clippy::cast_possible_truncation)] // whole and bounded, checked above
            let whole = cents as i64;
            *value = json!(whole);
        }
    }
}

/// `field` when it is missing or null.
fn fill(object: &mut Map<String, Json>, field: &str, default: Json) {
    if object.get(field).is_none_or(Json::is_null) {
        object.insert(field.to_owned(), default);
    }
}

/// `YYYY-MM-DD HH:MM` (seconds and a `T` accepted) in the house's zone, in
/// ms since the epoch. An hour skipped by a clock change reads as the hour
/// after; an hour that happens twice, as the first one.
pub(crate) fn local_ms(local: &str, tz: &jiff::tz::TimeZone) -> Option<u64> {
    let local = local.trim().replacen('T', " ", 1);
    let civil = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|format| jiff::civil::DateTime::strptime(format, &local).ok())?;
    let ms = civil
        .to_zoned(tz.clone())
        .ok()?
        .timestamp()
        .as_millisecond();
    u64::try_from(ms).ok()
}

/// The text without its long runs of digits.
fn scrub(text: &str) -> String {
    fn flush(out: &mut String, run: &mut String) {
        if run.len() >= LONG_NUMBER {
            out.push('…');
        } else {
            out.push_str(run);
        }
        run.clear();
    }
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else {
            flush(&mut out, &mut run);
            out.push(c);
        }
    }
    flush(&mut out, &mut run);
    out
}

/// A photo's type, by its first bytes (JPEG unless it says otherwise).
fn mime(photo: &[u8]) -> &'static str {
    if photo.starts_with(b"\x89PNG") {
        "image/png"
    } else if photo.len() >= 12 && &photo[..4] == b"RIFF" && &photo[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::llm::tests::fake_provider;

    fn hub() -> moli_runtime::Hub {
        moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap()
    }

    fn line(label: &str, product: Option<&str>) -> Json {
        json!({
            "label": label, "name": "Lait demi-écrémé", "qty": 2.0, "unit": null,
            "price_cents": 99, "discount_cents": 0, "line_cents": 198, "vat": 5.5,
            "aisle": "Crèmerie", "keep": true, "reason": "", "product": product,
            "new_product": null,
        })
    }

    fn reading() -> Json {
        json!({
            "provider": "enseigne-x", "store": "Ville-A", "at_local": "2026-10-28 12:37",
            "total_cents": 1047, "unreadable": false,
            "lines": [line("LAIT DEMI-ECR 1L", Some("lait")), line("SAC CABAS", None)],
        })
    }

    fn answer(content: &str) -> String {
        json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }],
            "usage": { "prompt_tokens": 3_000, "completion_tokens": 400 },
        })
        .to_string()
    }

    fn body_of(request: &str) -> Json {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn products() -> Vec<(String, String, Option<String>)> {
        vec![(
            "lait".into(),
            "Lait demi-écrémé".into(),
            Some("Crèmerie".into()),
        )]
    }

    fn chains() -> Vec<(String, String)> {
        vec![("enseigne-x".into(), "Enseigne-X".into())]
    }

    /// The single provider of an old `[assistant]`, at `url`.
    fn implicit(url: &str, extra: &str) -> Assistant {
        let config: Config = toml::from_str(&format!(
            "base_url = \"{url}\"\nmodel = \"gpt-4.1-mini\"\n{extra}"
        ))
        .unwrap();
        Assistant::new(hub(), None, None, None, config).unwrap()
    }

    fn photos(n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| [b"\xff\xd8photo-".as_slice(), i.to_string().as_bytes()].concat())
            .collect()
    }

    #[tokio::test]
    async fn a_receipt_is_read_with_the_photos_in_order() {
        let (url, asked) = fake_provider(vec![answer(&reading().to_string())]).await;
        let moli = implicit(&url, "");
        let photos = vec![b"\xff\xd8first".to_vec(), b"\xff\xd8second".to_vec()];
        let (read, reader) = moli
            .read_receipt(&photos, &products(), &chains())
            .await
            .unwrap();
        assert_eq!(reader, "default/gpt-4.1-mini");

        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].starts_with("POST /v1/chat/completions "));
        let body = body_of(&asked[0]);
        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        let content = body["messages"][1]["content"].as_array().unwrap();
        let images: Vec<&Json> = content
            .iter()
            .filter(|part| part["type"] == "image_url")
            .collect();
        assert_eq!(images.len(), 2);
        for (image, photo) in images.iter().zip(&photos) {
            assert_eq!(
                image["image_url"]["url"],
                format!("data:image/jpeg;base64,{}", base64(photo))
            );
            assert_eq!(image["image_url"]["detail"], "high");
        }
        // The products and the chains are said in words.
        let text = content
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<String>();
        assert!(text.contains("lait") && text.contains("Crèmerie"), "{text}");
        assert!(
            text.contains("enseigne-x") && text.contains("Enseigne-X"),
            "{text}"
        );
        let format = &body["response_format"];
        assert_eq!(format["type"], "json_schema");
        assert_eq!(format["json_schema"]["strict"], true);
        assert_eq!(format["json_schema"]["schema"], schema());
        assert_eq!(body["max_tokens"], 8_000);
        assert_eq!(body["temperature"], 0.2);
        // A reasoning model thinks within twice the room.
        let thinking = request("gpt-5-mini", "today", &photos, &[], &[]);
        assert_eq!(thinking["max_completion_tokens"], 16_000);
        assert_eq!(thinking["reasoning_effort"], "low");
        assert!(thinking.get("max_tokens").is_none());

        // The reading has the draft's shape: `at` in ms, no `at_local`.
        assert_eq!(read["at"], 1_793_187_420_000_u64);
        assert!(read.get("at_local").is_none());
        assert_eq!(read["provider"], "enseigne-x");
        assert_eq!(read["store"], "Ville-A");
        assert_eq!(read["lines"].as_array().unwrap().len(), 2);
        assert_eq!(read["lines"][0]["product"], "lait");
    }

    #[tokio::test]
    async fn a_provider_below_tier_one_never_sees_a_receipt() {
        let (home, home_asked) = fake_provider(vec![answer(&reading().to_string())]).await;
        let (far, far_asked) = fake_provider(vec![answer(&reading().to_string())]).await;
        let config: Config = toml::from_str(&format!(
            "[[provider]]\nid = \"home\"\nbase_url = \"{home}\"\nmodel = \"small\"\ntier = 1\n\n\
             [[provider]]\nid = \"far\"\nbase_url = \"{far}\"\nmodel = \"vision\"\ntier = 3\n\n\
             [routes]\nchat = \"home\"\nreceipts = \"far\"\n"
        ))
        .unwrap();
        let moli = Assistant::new(hub(), None, None, None, config).unwrap();
        let reader = moli.receipt_reader();
        assert_eq!(
            (reader.provider.as_str(), reader.model.as_str()),
            ("far", "vision")
        );
        assert!(!reader.allowed);
        assert!(reader.reason.is_some());
        let err = moli
            .read_receipt(&photos(1), &products(), &chains())
            .await
            .unwrap_err();
        assert!(matches!(err, AssistantError::NotConfigured(_)), "{err}");
        tokio::task::yield_now().await;
        assert!(far_asked.lock().unwrap().is_empty(), "zero request");
        assert!(home_asked.lock().unwrap().is_empty(), "no fallback either");
    }

    #[tokio::test]
    async fn receipts_model_overrides_the_model_in_implicit_mode() {
        let (url, asked) = fake_provider(vec![answer(&reading().to_string())]).await;
        let moli = implicit(&url, "receipts_model = \"gpt-4.1\"\n");
        let reader = moli.receipt_reader();
        assert_eq!(
            (reader.provider.as_str(), reader.model.as_str()),
            ("default", "gpt-4.1")
        );
        assert!(reader.allowed, "{:?}", reader.reason);
        let (_, name) = moli
            .read_receipt(&photos(1), &products(), &chains())
            .await
            .unwrap();
        assert_eq!(name, "default/gpt-4.1");
        assert_eq!(body_of(&asked.lock().unwrap()[0])["model"], "gpt-4.1");
        // Empty: the conversation's model.
        let (url, _) = fake_provider(Vec::new()).await;
        assert_eq!(implicit(&url, "").receipt_reader().model, "gpt-4.1-mini");
    }

    #[tokio::test]
    async fn five_photos_are_refused() {
        let (url, asked) = fake_provider(vec![answer(&reading().to_string())]).await;
        let moli = implicit(&url, "");
        for n in [0, 5] {
            let err = moli
                .read_receipt(&photos(n), &products(), &chains())
                .await
                .unwrap_err();
            assert!(matches!(err, AssistantError::Invalid(_)), "{n}: {err}");
        }
        tokio::task::yield_now().await;
        assert!(asked.lock().unwrap().is_empty(), "zero request");
    }

    #[tokio::test]
    async fn an_answer_that_is_not_json_is_an_upstream_error() {
        let (url, _) = fake_provider(vec![answer("Je ne peux pas lire ce ticket.")]).await;
        let moli = implicit(&url, "");
        let err = moli
            .read_receipt(&photos(1), &products(), &chains())
            .await
            .unwrap_err();
        assert!(matches!(err, AssistantError::Upstream(_)), "{err}");
        // Paid for all the same, and still a receipt, not a conversation.
        let kept = moli.exchanges(1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].kind, Kind::Receipt);
        assert_eq!(kept[0].reply, "lecture impossible");
        assert!(kept[0].cost.total > 0.0);
    }

    #[tokio::test]
    async fn the_cost_is_logged_as_a_receipt_exchange() {
        let (url, _) = fake_provider(vec![answer(&reading().to_string())]).await;
        let moli = implicit(&url, "");
        moli.read_receipt(&photos(2), &products(), &chains())
            .await
            .unwrap();
        let kept = moli.exchanges(1);
        assert_eq!(kept.len(), 1);
        let e = &kept[0];
        assert_eq!(e.kind, Kind::Receipt);
        assert_eq!(e.question, "ticket de caisse");
        assert!(e.reply.contains('2'), "{}", e.reply);
        let usage = json!({ "prompt_tokens": 3_000, "completion_tokens": 400 });
        let expected = crate::pricing::chat("gpt-4.1-mini", &usage);
        assert!(expected > 0.0);
        assert!((e.cost.total - expected).abs() < 1e-12, "{:?}", e.cost);
    }

    #[test]
    fn the_local_time_is_converted_with_the_house_zone() {
        let paris = jiff::tz::TimeZone::get("Europe/Paris").unwrap();
        assert_eq!(
            local_ms("2026-10-28 12:37", &paris),
            Some(1_793_187_420_000)
        );
        // Seconds, or a `T`, as some models write it.
        for written in [
            "2026-10-28 12:37:00",
            "2026-10-28T12:37",
            "2026-10-28T12:37:00",
        ] {
            assert_eq!(
                local_ms(written, &paris),
                Some(1_793_187_420_000),
                "{written}"
            );
        }
        // 02:30 never happened on 29 March: the hour after (03:30 CEST, 01:30 UTC).
        assert_eq!(
            local_ms("2026-03-29 02:30", &paris),
            Some(1_774_747_800_000)
        );
        // 02:30 happened twice on 25 October: the first one (CEST, 00:30 UTC).
        assert_eq!(
            local_ms("2026-10-25 02:30", &paris),
            Some(1_792_888_200_000)
        );
        assert_eq!(local_ms("28/10/2026", &paris), None);
    }

    #[test]
    fn the_schema_names_the_fields_of_the_pantry_draft() {
        fn names(object: &Json) -> Vec<String> {
            let mut names: Vec<String> = object["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            names.sort();
            let mut required: Vec<String> = object["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n.as_str().unwrap().to_owned())
                .collect();
            required.sort();
            assert_eq!(names, required, "every property is required");
            assert_eq!(object["additionalProperties"], false);
            names
        }
        let schema = schema();
        let mut draft = vec![
            "provider",
            "store",
            "at_local",
            "total_cents",
            "lines",
            "unreadable",
        ];
        draft.sort_unstable();
        assert_eq!(names(&schema), draft);
        let mut line = vec![
            "label",
            "name",
            "qty",
            "unit",
            "price_cents",
            "discount_cents",
            "line_cents",
            "vat",
            "aisle",
            "keep",
            "reason",
            "product",
            "new_product",
        ];
        line.sort_unstable();
        assert_eq!(names(&schema["properties"]["lines"]["items"]), line);
        // The optional ones may be null, the others may not.
        assert_eq!(
            schema["properties"]["at_local"]["type"],
            json!(["string", "null"])
        );
        assert_eq!(schema["properties"]["store"]["type"], "string");
        let item = &schema["properties"]["lines"]["items"]["properties"];
        assert_eq!(item["product"]["type"], json!(["string", "null"]));
        assert_eq!(item["keep"]["type"], "boolean");
    }

    #[test]
    fn a_reading_is_tidied_against_what_the_pantry_knows() {
        let paris = jiff::tz::TimeZone::get("Europe/Paris").unwrap();
        let mut raw = reading();
        raw["provider"] = json!("ENSEIGNE-X");
        raw["lines"][1] = line("SAC CABAS AUTO 4012345678", Some("sac-inconnu"));
        raw["lines"][1]["name"] = json!("Sac cabas");
        let fenced = format!("```json\n{raw}\n```");
        let read = tidy(&fenced, &products(), &chains(), &paris).unwrap();
        // A chain's printed name becomes its id.
        assert_eq!(read["provider"], "enseigne-x");
        // A product id the pantry does not have becomes a product to create.
        assert_eq!(read["lines"][1]["product"], Json::Null);
        assert_eq!(read["lines"][1]["new_product"], "Sac cabas");
        // A long number is never kept.
        assert_eq!(read["lines"][1]["label"], "SAC CABAS AUTO …");
        // A chain nobody knows keeps its printed name.
        let mut other = reading();
        other["provider"] = json!("Enseigne-Z");
        let read = tidy(&other.to_string(), &products(), &chains(), &paris).unwrap();
        assert_eq!(read["provider"], "Enseigne-Z");
        // Unreadable: no lines.
        let mut blurred = reading();
        blurred["unreadable"] = json!(true);
        let read = tidy(&blurred.to_string(), &products(), &chains(), &paris).unwrap();
        assert!(read["lines"].as_array().unwrap().is_empty());
        // Not an object, or no lines: refused.
        assert!(tidy("[1, 2]", &products(), &chains(), &paris).is_err());
        assert!(tidy("{\"provider\":\"x\"}", &products(), &chains(), &paris).is_err());
    }

    /// A model that does not hold to the schema still gives a draft.
    #[test]
    fn missing_fields_take_the_drafts_defaults() {
        let paris = jiff::tz::TimeZone::get("Europe/Paris").unwrap();
        let sparse = json!({
            "provider": null,
            "lines": [{ "label": "PAIN", "name": "Pain", "qty": 1, "price_cents": 120, "line_cents": 120 }],
        });
        let read = tidy(&sparse.to_string(), &products(), &chains(), &paris).unwrap();
        assert_eq!(read["provider"], "");
        assert_eq!(read["store"], "");
        assert_eq!(read["total_cents"], Json::Null);
        assert_eq!(read["unreadable"], false);
        assert_eq!(read["at"], Json::Null);
        let line = &read["lines"][0];
        assert_eq!(line["keep"], true);
        assert_eq!(line["reason"], "");
        assert_eq!(line["discount_cents"], 0);
        // What the model said is kept.
        let mut said = sparse;
        said["lines"][0]["keep"] = json!(false);
        said["lines"][0]["discount_cents"] = json!(30);
        let read = tidy(&said.to_string(), &products(), &chains(), &paris).unwrap();
        assert_eq!(read["lines"][0]["keep"], false);
        assert_eq!(read["lines"][0]["discount_cents"], 30);
    }

    /// A reading the model wrote with floats for the amounts and without the
    /// labels is still a reading that fits the draft (it was paid for).
    #[test]
    fn whole_floats_and_missing_labels_still_fit_the_draft() {
        let paris = jiff::tz::TimeZone::get("Europe/Paris").unwrap();
        let loose = json!({
            "total_cents": 1047.0,
            "lines": [{
                "qty": 2.0, "price_cents": 99.0, "line_cents": 198.0,
                "discount_cents": 0.0, "keep": true,
            }, {
                "label": "X", "name": "x", "qty": 1, "price_cents": 10.5, "line_cents": 10,
            }],
        });
        let read = tidy(&loose.to_string(), &products(), &chains(), &paris).unwrap();
        assert_eq!(read["total_cents"], json!(1047));
        assert!(read["total_cents"].is_i64());
        let line = &read["lines"][0];
        for field in ["price_cents", "line_cents", "discount_cents"] {
            assert!(line[field].is_i64(), "{field}: {line}");
        }
        assert_eq!(line["price_cents"], 99);
        assert_eq!(
            (line["label"].as_str(), line["name"].as_str()),
            (Some(""), Some(""))
        );
        // A real fraction of a cent is left as it is (it is not a whole amount).
        assert!(read["lines"][1]["price_cents"].is_f64());
    }

    #[test]
    fn photos_are_sent_in_standard_base64_with_their_type() {
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(mime(b"\xff\xd8\xff\xe0"), "image/jpeg");
        assert_eq!(mime(b"\x89PNG\r\n\x1a\n"), "image/png");
        assert_eq!(mime(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
    }
}
