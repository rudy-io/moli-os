// The app's own words (the dashboard has its own translations, in Moli).
// French first, English for every other language.

import { getLocales } from "expo-localization";

const fr = {
  welcomeTitle: "Bienvenue chez Moli",
  welcomeText:
    "Indique l'adresse de ta maison Moli : celle que tu ouvres dans ton navigateur. Sur le Wi-Fi de la maison, l'adresse locale suffit.",
  addressLabel: "Adresse de ta maison",
  addressPlaceholder: "maison.exemple.fr ou 192.168.1.x:8790",
  connect: "Se connecter",
  checking: "Je frappe à la porte…",
  badAddress: "Cette adresse ne ressemble pas à une adresse web.",
  unreachable: "Personne ne répond à cette adresse. Vérifie-la, et que tu es bien connecté (Wi-Fi de la maison pour une adresse locale).",
  notMoli: "Quelque chose répond, mais ce n'est pas un Moli (ou il est caché derrière une autre connexion).",
  notMoliTitle: "Ce n'est pas un Moli",
  refused:
    "C'est bien un Moli, mais il n'ouvre qu'au Wi-Fi de la maison ou à travers Cloudflare Access. Connecte-toi au Wi-Fi de la maison, ou demande à la personne qui l'a installé.",
  insecure: "Hors de la maison, l'adresse doit commencer par https:// : sans chiffrement, tout passerait en clair.",
  openAnyway: "Ouvrir quand même",
  help: "Pas encore de Moli ? Il s'installe sur un petit ordinateur de la maison : moli-os sur GitHub.",
  unreachableTitle: "Moli est injoignable",
  unreachableSub: "Vérifie ta connexion, puis réessaie.",
  retry: "Réessayer",
  brokenTitle: "Quelque chose a coincé",
  brokenSub: "L'appli s'est arrêtée sur cet écran. Réessaie, ou choisis à nouveau ta maison.",
  changeHouse: "Changer de maison",
  leaveTitle: "Changer de maison ?",
  leaveText: "Ce téléphone ne donnera plus de nouvelles à cette maison (position, batterie). Tu pourras la retrouver en retapant son adresse.",
  leave: "Changer",
  cancel: "Annuler",
  locationTitle: "Moli et ta position",
  locationText:
    "Pour savoir quand tu arrives à la maison ou que tu en pars, même appli fermée, Moli a besoin de ta position. Elle n'est envoyée qu'à ton Moli, chez toi. Choisis « Toujours » à la deuxième question.",
  later: "Plus tard",
  continue: "Continuer",
  phone: "Téléphone",
  fingerprintKeep: "Retenir le code de la maison",
  fingerprintUnlock: "Code de la maison",
  fingerprintOfferTitle: "Utiliser l'empreinte ?",
  fingerprintOfferText: "La prochaine fois, ton empreinte remplacera le code de la maison sur ce téléphone.",
  fingerprintNo: "Non merci",
  fingerprintYes: "Oui",
  updateTitle: "Nouvelle version de Moli",
  updateText: "Une mise à jour de l'appli est prête sur ta maison. L'installer maintenant ?",
  updateInstall: "Installer",
  updatePermTitle: "Autoriser les mises à jour",
  updatePermText:
    "Android demande d'autoriser Moli à installer ses propres mises à jour. Active « Autoriser cette source », puis reviens dans Moli.",
  updateDownloading: "Téléchargement de la mise à jour…",
  updateFailed: "La mise à jour n'a pas pu s'installer. Réessaie plus tard.",
  googleFailed: "La connexion avec Google n'a pas abouti. Réessaie, ou choisis le code par e-mail.",
};

const en: typeof fr = {
  welcomeTitle: "Welcome to Moli",
  welcomeText:
    "Enter your Moli house's address: the one you open in your browser. On the house's Wi-Fi, the local address is enough.",
  addressLabel: "Your house's address",
  addressPlaceholder: "home.example.org or 192.168.1.x:8790",
  connect: "Connect",
  checking: "Knocking at the door…",
  badAddress: "This doesn't look like a web address.",
  unreachable: "Nobody answers at this address. Check it, and that you are connected (the house's Wi-Fi for a local address).",
  notMoli: "Something answers, but it isn't a Moli (or it hides behind another sign-in).",
  notMoliTitle: "This isn't a Moli",
  refused:
    "This is a Moli, but it only lets in the house's Wi-Fi or Cloudflare Access. Join the house's Wi-Fi, or ask whoever set it up.",
  insecure: "Away from home, the address must start with https://: without encryption, everything would travel in the clear.",
  openAnyway: "Open anyway",
  help: "No Moli yet? It runs on a small computer at home: moli-os on GitHub.",
  unreachableTitle: "Moli can't be reached",
  unreachableSub: "Check your connection, then try again.",
  retry: "Try again",
  brokenTitle: "Something got stuck",
  brokenSub: "The app stopped on this screen. Try again, or choose your house again.",
  changeHouse: "Change house",
  leaveTitle: "Change house?",
  leaveText: "This phone will stop reporting to this house (location, battery). You can come back by typing its address again.",
  leave: "Change",
  cancel: "Cancel",
  locationTitle: "Moli and your location",
  locationText:
    "To know when you arrive home or leave, even with the app closed, Moli needs your location. It is only sent to your own Moli, at home. Choose “Always” at the second question.",
  later: "Later",
  continue: "Continue",
  phone: "Phone",
  fingerprintKeep: "Remember the house code",
  fingerprintUnlock: "House code",
  fingerprintOfferTitle: "Use your fingerprint?",
  fingerprintOfferText: "Next time, your fingerprint will replace the house code on this phone.",
  fingerprintNo: "No thanks",
  fingerprintYes: "Yes",
  updateTitle: "New version of Moli",
  updateText: "An update of the app is ready on your house. Install it now?",
  updateInstall: "Install",
  updatePermTitle: "Allow updates",
  updatePermText:
    "Android asks to let Moli install its own updates. Turn on “Allow from this source”, then come back to Moli.",
  updateDownloading: "Downloading the update…",
  updateFailed: "The update could not be installed. Try again later.",
  googleFailed: "Signing in with Google did not work. Try again, or choose the e-mail code.",
};

const lang = getLocales()[0]?.languageCode ?? "fr";

export const t = lang === "fr" ? fr : en;
