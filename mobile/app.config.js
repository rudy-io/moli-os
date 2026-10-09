// The app is one for every house: the person types their house's address on
// the first screen (src/house.ts). The store identity (ids, Expo account)
// is the publisher's, not the project's: it comes from `house.json` next to
// this file (never committed; model: house.example.json).
//
// A build made for a single house may also preset its address (house.json
// `url`, or MOLI_URL): the first screen is then skipped. Never in a build
// meant for the stores: everyone would get that house's address.
const fs = require("fs");
const path = require("path");

module.exports = ({ config }) => {
  const file = path.join(__dirname, "house.json");
  const house = fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : {};
  const url = house.url ?? process.env.MOLI_URL ?? null;
  return {
    ...config,
    ...(house.owner && { owner: house.owner }),
    ios: {
      ...config.ios,
      ...(house.bundleId && { bundleIdentifier: house.bundleId }),
      ...(house.appleTeamId && { appleTeamId: house.appleTeamId }),
    },
    android: { ...config.android, ...(house.bundleId && { package: house.bundleId }) },
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
