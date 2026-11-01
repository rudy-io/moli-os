//! The Moli app on the household's phones (crate `moli-phones`).
//!
//! - `POST /api/mobile/register` (a person of the house: the code, or someone
//!   Moli knows by Cloudflare Access or a password, never a guest; this very
//!   machine too): `{ name, platform, model, push_token? }` → `{ id, token,
//!   home }`; the token is shown once. The house hears of every phone paired.
//! - `POST /api/phones/<id>/report` (`Authorization: Bearer <token>`, no
//!   Cloudflare login needed: see `caller.rs`): a `Report` → `{ home }`;
//!   refused from the phone of someone no longer of the house (removed, a
//!   guest whose stay ended).

use axum::Extension;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use moli_runtime::Hub;
use serde::Deserialize;
use serde_json::json;

use crate::caller::{Caller, Via};

#[derive(Clone)]
pub(crate) struct PhonesGate(pub(crate) Option<moli_phones::Gateway>);

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Pairing {
    name: String,
    platform: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    push_token: Option<String>,
}

/// Who paired a phone, for the house: the person, else the e-mail Access
/// vouched for, else the address.
fn paired_by(caller: &Caller) -> String {
    if let Some(person) = &caller.person {
        return person.name.clone();
    }
    match &caller.via {
        Via::Access { email } => email.clone(),
        _ => caller.key.clone(),
    }
}

/// Whether `caller` may pair a phone. A paired phone hears the house from
/// anywhere: being on the home network is not enough, a person must stand
/// behind it (`human`: the code), or Moli must know who it is.
fn may_pair(caller: &Caller, human: bool) -> bool {
    human || caller.person.is_some() || caller.is_local_machine()
}

pub(crate) async fn register(
    State(hub): State<Hub>,
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(humans): Extension<std::sync::Arc<crate::session::Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    axum::Json(p): axum::Json<Pairing>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.telephone.foyer_seulement"),
        );
    }
    // A paired phone hears every notification of the house: not a guest's.
    if caller
        .person
        .as_ref()
        .is_some_and(crate::people::PersonRef::is_guest)
    {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.personnes.invite_limite"),
        );
    }
    if !may_pair(&caller, humans.is_human(&headers, &caller.key)) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.telephone.personne_seulement"),
        );
    }
    // Paired from a person's session: the phone is theirs.
    let person = caller.person.as_ref().map(|p| p.id.as_str());
    match gate.register(
        &p.name,
        &p.platform,
        &p.model,
        p.push_token.as_deref(),
        person,
    ) {
        Ok(paired) => {
            // It will hear every notice of the house: the house hears of it.
            // Its name as kept, last: the pairer chose it, it says nothing
            // for sure.
            let name = gate
                .list()
                .into_iter()
                .find(|phone| phone.id == paired.id)
                .map(|phone| phone.name)
                .unwrap_or_default();
            hub.notice(moli_core::Notice {
                title: Some(moli_i18n::tr!("serveur.telephone.notice_titre")),
                message: moli_i18n::tr!(
                    "serveur.telephone.notice_appaire",
                    by = paired_by(&caller),
                    id = paired.id,
                    name = name
                ),
                from: Some("Moli".into()),
                ts: moli_core::now_ms(),
                to: None,
            });
            axum::Json(paired).into_response()
        }
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &format!("{e:#}")),
    }
}

/// Forgets a phone (a person, with the PIN: a lost phone is cut off).
pub(crate) async fn remove(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(humans): Extension<std::sync::Arc<crate::session::Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    if !humans.is_human(&headers, &caller.key) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.telephone.retrait_humain"),
        );
    }
    match gate.remove(&id) {
        Ok(true) => axum::Json(json!({ "removed": id })).into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.telephone.inconnu"),
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

pub(crate) async fn report(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Path(id): Path<String>,
    headers: HeaderMap,
    axum::Json(r): axum::Json<moli_phones::Report>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !gate.verify(&id, token.trim()) {
        return error(
            StatusCode::UNAUTHORIZED,
            &moli_i18n::tr!("serveur.telephone.jeton_refuse"),
        );
    }
    // Its person removed, or a guest whose stay ended: it says nothing more.
    if !gate.belongs(&id) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("pilotes.phones.hors_maison"),
        );
    }
    match gate.report(&id, r) {
        Ok(home) => axum::Json(json!({ "home": home })).into_response(),
        Err(e) => error(StatusCode::UNPROCESSABLE_ENTITY, &format!("{e:#}")),
    }
}

/// The household's phones, and whose each is.
pub(crate) async fn list(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    caller: Caller,
) -> Response {
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.acces.refuse"),
        );
    }
    axum::Json(json!({ "phones": gate.map(|g| g.list()).unwrap_or_default() })).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Owner {
    person: Option<String>,
}

/// Gives a phone to a person (an owner of the house). Under `/api/mobile/`:
/// a house behind Cloudflare Access lets `/api/phones` through unsigned
/// (the phones' reports), so an owner's order must not live there.
pub(crate) async fn assign(
    Extension(PhonesGate(gate)): Extension<PhonesGate>,
    Extension(humans): Extension<std::sync::Arc<crate::session::Sessions>>,
    Extension(persons): Extension<std::sync::Arc<crate::people::People>>,
    caller: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
    axum::Json(body): axum::Json<Owner>,
) -> Response {
    let Some(gate) = gate else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.pilote.absent", pilote = "phones"),
        );
    };
    if !crate::people_api::is_owner(&caller, &humans, &persons, &headers) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.personnes.proprietaire_seul"),
        );
    }
    if let Some(p) = &body.person
        && persons.get(p).is_none()
    {
        return error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.personnes.inconnue"),
        );
    }
    match gate.assign(&id, body.person.as_deref()) {
        Ok(true) => axum::Json(json!({ "phone": id, "person": body.person })).into_response(),
        Ok(false) => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.telephone.inconnu"),
        ),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::{PersonRef, Role};

    fn caller(via: Via, person: bool) -> Caller {
        Caller {
            via,
            key: "10.0.0.30".into(),
            person: person.then(|| PersonRef {
                id: "sam".into(),
                name: "Sam".into(),
                role: Role::Member,
                rooms: Vec::new(),
            }),
        }
    }

    #[test]
    fn a_phone_is_paired_by_a_person() {
        // A device of the home network, nobody known: not without the code.
        assert!(!may_pair(&caller(Via::Lan, false), false));
        assert!(may_pair(&caller(Via::Lan, false), true));
        // Signed in with a password, or a person Access vouched for.
        assert!(may_pair(&caller(Via::Lan, true), false));
        let access = Via::Access {
            email: "sam@example.com".into(),
        };
        assert!(may_pair(&caller(access.clone(), true), false));
        // An e-mail Access let in but the house does not know: the code.
        assert!(!may_pair(&caller(access, false), false));
        // This very machine.
        assert!(may_pair(&caller(Via::Loopback, false), false));
    }
}
