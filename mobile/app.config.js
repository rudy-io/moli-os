// The app is one for every house: the person types their house's address on
// the first screen (src/house.ts). The store identity (ids, Expo account)
// is the publisher's, not the project's: it comes from `house.json` next to
// this file (never committed; model: house.example.json).
//
// On EAS's servers, house.json is not there (git leaves it out of the upload):
// give it as a file environment variable, MOLI_HOUSE, and Android's Firebase
// file (google-services.json, needed for notifications) as GOOGLE_SERVICES_JSON.
// Locally, both files simply sit next to this one.
//
// A build made for a single house may also preset its address (house.json
// `url`, or MOLI_URL): the first screen is then skipped. Never in a build
// meant for the stores: everyone would get that house's address.
const fs = require("fs");
const path = require("path");

// Each build a higher number than the last (Android's versionCode): the
// house offers an update only to an older app (src/update.ts). Minutes since
// 2026 (MOLI_VERSION_CODE to force one): always growing, far below the limit.
const buildNumber = () =>
  Number(process.env.MOLI_VERSION_CODE) || Math.floor((Date.now() - Date.UTC(2026, 0, 1)) / 60000);

module.exports = ({ config }) => {
  const file = process.env.MOLI_HOUSE ?? path.join(__dirname, "house.json");
  const house = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : {};
  const url = house.url ?? process.env.MOLI_URL ?? null;
  const local = path.join(__dirname, "google-services.json");
  const google = process.env.GOOGLE_SERVICES_JSON ?? (fs.existsSync(local) ? local : null);
  return {
    ...config,
    ...(house.owner && { owner: house.owner }),
    ios: {
      ...config.ios,
      ...(house.bundleId && { bundleIdentifier: house.bundleId }),
      ...(house.appleTeamId && { appleTeamId: house.appleTeamId }),
    },
    android: {
      ...config.android,
      versionCode: buildNumber(),
      ...(house.bundleId && { package: house.bundleId }),
      ...(google && { googleServicesFile: google }),
    },
    extra: {
      ...config.extra,
      // Only when there is one: a null here reached the build as {} (build 7
      // crashed at launch, taking it for an address).
      ...(url && { moliUrl: url }),
      // More domains the dashboard may move to without leaving the app, on
      // top of the house's own and Cloudflare Access's sign-in (e.g. another
      // sign-in page in front of the house).
      allowedSuffixes: house.allowedSuffixes ?? [],
      ...(house.easProjectId && { eas: { projectId: house.easProjectId } }),
    },
  };
};
