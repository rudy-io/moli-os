//! `/api/people`, `/api/zones`, `/api/invitation`: the household's people.
//!
//! - `GET /api/people`: who lives here and the zones (everyone in the
//!   household); an owner also sees e-mails, rooms, ends and passwords set.
//! - `POST /api/people`, `PUT|DELETE /api/people/{id}`: an owner.
//! - `POST /api/people/{id}/invite`: an owner → `{code, expires}` (7 days).
//! - `PUT /api/people/{id}/password`: the person themself → new password.
//! - `PUT /api/zones`: an owner (the whole list).
//! - `POST /api/invitation {code, password}`: anyone (a stranger too):
//!   behind the same lockouts as the house's code; sets the password and
//!   signs the person in.

use std::sync::Arc;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use crate::caller::{Caller, Via};
use crate::people::{Draft, People, Refused, Zone};
use crate::session::{LoginError, Sessions};

type Persons = Extension<Arc<People>>;

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn refused(r: Refused) -> Response {
    match r {
        Refused::Invalid(why) => error(StatusCode::UNPROCESSABLE_ENTITY, &why),
        Refused::NotFound => error(
            StatusCode::NOT_FOUND,
            &moli_i18n::tr!("serveur.personnes.inconnue"),
        ),
        Refused::LastOwner => error(
            StatusCode::CONFLICT,
            &moli_i18n::tr!("serveur.personnes.dernier_proprietaire"),
        ),
        Refused::Store => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &moli_i18n::tr!("serveur.personnes.pas_ecrit"),
        ),
    }
}

/// An owner: a person with that role, or an owner Cloudflare Access names
/// in the configuration; in a house without any owner yet, a person who
/// proved the house's code (the first owner is created that way).
pub(crate) fn is_owner(
    caller: &Caller,
    humans: &Sessions,
    persons: &People,
    headers: &axum::http::HeaderMap,
) -> bool {
    if caller
        .person
        .as_ref()
        .is_some_and(crate::people::PersonRef::is_owner)
    {
        return true;
    }
    if let Via::Access { email } = &caller.via
        && humans.is_owner(email)
    {
        return true;
    }
    !persons.has_owner()
        && !humans.has_owners()
        && caller.is_household()
        && humans.is_human(headers, &caller.key)
}

fn owners_only() -> Response {
    error(
        StatusCode::FORBIDDEN,
        &moli_i18n::tr!("serveur.personnes.proprietaire_seul"),
    )
}

pub(crate) async fn list(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    headers: axum::http::HeaderMap,
) -> Response {
    if !caller.is_household() {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.acces.refuse"),
        );
    }
    let owner = is_owner(&caller, &humans, &persons, &headers);
    let book = persons.book();
    let people: Vec<serde_json::Value> = book
        .people
        .iter()
        .map(|p| {
            let mut v = json!({ "id": p.id, "name": p.name, "role": p.role, "color": p.color });
            if owner {
                v["emails"] = json!(p.emails);
                v["rooms"] = json!(p.rooms);
                v["expires"] = json!(p.expires);
                v["macs"] = json!(p.macs);
                v["has_password"] = json!(persons.has_password(&p.id));
                v["created"] = json!(p.created);
            }
            v
        })
        .collect();
    Json(json!({
        "me": caller.person,
        "owner": owner,
        "people": people,
        "zones": book.zones,
    }))
    .into_response()
}

pub(crate) async fn create(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    headers: axum::http::HeaderMap,
    Json(draft): Json<Draft>,
) -> Response {
    if !is_owner(&caller, &humans, &persons, &headers) {
        return owners_only();
    }
    match persons.create(draft).await {
        Ok(p) => (StatusCode::CREATED, Json(p)).into_response(),
        Err(r) => refused(r),
    }
}

pub(crate) async fn update(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Json(draft): Json<Draft>,
) -> Response {
    if !is_owner(&caller, &humans, &persons, &headers) {
        return owners_only();
    }
    match persons.update(&id, draft).await {
        Ok(p) => Json(p).into_response(),
        Err(r) => refused(r),
    }
}

pub(crate) async fn remove(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !is_owner(&caller, &humans, &persons, &headers) {
        return owners_only();
    }
    match persons.remove(&id).await {
        Ok(()) => Json(json!({ "removed": id })).into_response(),
        Err(r) => refused(r),
    }
}

pub(crate) async fn invite(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !is_owner(&caller, &humans, &persons, &headers) {
        return owners_only();
    }
    match persons.invite(&id).await {
        Ok((code, expires)) => Json(json!({ "code": code, "expires": expires })).into_response(),
        Err(r) => refused(r),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordBody {
    password: String,
}

/// A person changes their own password (an owner resets someone else's by
/// a new invitation).
pub(crate) async fn password(
    Extension(persons): Persons,
    caller: Caller,
    Path(id): Path<String>,
    Json(body): Json<PasswordBody>,
) -> Response {
    if caller.person.as_ref().is_none_or(|p| p.id != id) {
        return error(
            StatusCode::FORBIDDEN,
            &moli_i18n::tr!("serveur.personnes.soi_seulement"),
        );
    }
    match persons.set_password(&id, &body.password).await {
        // Every session of this person ended: sign in again on this one.
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(r) => refused(r),
    }
}

pub(crate) async fn zones(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    headers: axum::http::HeaderMap,
    Json(zones): Json<Vec<Zone>>,
) -> Response {
    if !is_owner(&caller, &humans, &persons, &headers) {
        return owners_only();
    }
    match persons.set_zones(zones).await {
        Ok(z) => Json(z).into_response(),
        Err(r) => refused(r),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvitationBody {
    code: String,
    password: String,
}

pub(crate) async fn redeem(
    Extension(humans): Extension<Arc<Sessions>>,
    Extension(persons): Persons,
    caller: Caller,
    Json(body): Json<InvitationBody>,
) -> Response {
    match persons
        .redeem(&humans, &body.code, &body.password, &caller.key)
        .await
    {
        Ok((token, person)) => (
            [(SET_COOKIE, People::cookie(&token))],
            Json(json!({ "person": person })),
        )
            .into_response(),
        Err(Ok(e)) => login_error(e, "serveur.personnes.invitation_incorrecte"),
        Err(Err(r)) => refused(r),
    }
}

/// A sign-in that did not go through, said the same way everywhere.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn login_error(e: LoginError, wrong: &str) -> Response {
    match e {
        LoginError::Wrong => error(StatusCode::UNAUTHORIZED, &moli_i18n::tr(wrong)),
        LoginError::Locked => error(
            StatusCode::TOO_MANY_REQUESTS,
            &moli_i18n::tr!("serveur.code.trop_d_essais"),
        ),
        LoginError::Disabled => error(
            StatusCode::SERVICE_UNAVAILABLE,
            &moli_i18n::tr!("serveur.code.installer"),
        ),
    }
}
