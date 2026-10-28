//! The household as the assistant may touch it (`moli_assistant::Directory`): add
//! a member by name, take back an addition nobody built on.

use std::sync::Arc;

use moli_assistant::{Directory, DirectoryDone};

use crate::people::{Draft, People, Refused, Role};

#[derive(Debug)]
pub(crate) struct PeopleDirectory(pub(crate) Arc<People>);

fn why(r: &Refused) -> String {
    match r {
        Refused::Invalid(why) => why.clone(),
        Refused::NotFound => moli_i18n::tr!("serveur.personnes.inconnue"),
        Refused::LastOwner => moli_i18n::tr!("serveur.personnes.dernier_proprietaire"),
        Refused::Store => moli_i18n::tr!("serveur.personnes.pas_ecrit"),
    }
}

impl Directory for PeopleDirectory {
    fn names(&self) -> Vec<String> {
        self.0.names()
    }

    fn add<'a>(&'a self, name: &'a str) -> DirectoryDone<'a, String> {
        Box::pin(async move {
            let draft = Draft {
                name: name.to_owned(),
                role: Role::Member,
                emails: Vec::new(),
                color: None,
                rooms: Vec::new(),
                expires: None,
                macs: Vec::new(),
            };
            // Two Léa would be two persons nobody tells apart.
            if self
                .0
                .names()
                .iter()
                .any(|n| n.to_lowercase() == name.to_lowercase())
            {
                return Err(moli_i18n::tr!("assistant.house.person_known", name = name));
            }
            self.0
                .create(draft)
                .await
                .map(|p| p.id)
                .map_err(|r| why(&r))
        })
    }

    fn remove_unused<'a>(&'a self, id: &'a str, name: &'a str) -> DirectoryDone<'a, ()> {
        Box::pin(async move { self.0.remove_unused(id, name).await.map_err(|r| why(&r)) })
    }
}
