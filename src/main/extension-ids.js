'use strict';
// Browser extensions that are allowed to ask to be paired with the app.
//  - The first ID is this project's own extension; it is fixed by the "key" field in extension/manifest.json,
//    so it stays the same wherever the extension folder lives.
//  - When you publish the extension on a store, add the ID the store gives it here. The Chrome Web Store and
//    Edge Add-ons each assign their own ID.
//  - If native messaging is added later, its host manifest ("allowed_origins") should be built from this same list.
const ALLOWED_EXTENSION_IDS = ['hlceennagceocjoffimpobpeidkkmpnc'];

module.exports = { ALLOWED_EXTENSION_IDS };
