# macOS Keychain limitation

Mapbrowser currently bundles CEF 152.0.6 through the Rust `cef` 152.2.0 bindings. Chromium's macOS encryption code uses the Keychain service name `Chromium Safe Storage` and account name `Chromium`. It retrieves the secret in that item, or creates a random secret if the item does not exist. Chromium uses it to protect persisted browser data. [Chromium 152 implementation](https://github.com/chromium/chromium/blob/152.0.7977.83/components/os_crypt/common/keychain_password_mac.mm)

If that item already exists and Mapbrowser is not authorized to access it, macOS can show the permission dialog seen during testing. The dialog names that specific encryption secret. It is not a request to import another browser's passwords or profiles. Rebuilding this locally signed prototype can also affect whether macOS recognizes its authorization.

Choose **Deny** if you do not want Mapbrowser to access the shared item. The app must not depend on encrypted cookies or login sessions persisting when access is unavailable. Login/session persistence remains a verification requirement; a successful build alone does not establish it.

## Profiles and encryption keys

Each space uses its own CEF request context and profile directory. This separates its cookies and site storage from other spaces. That separation does not give each space a different encryption key. The current CEF runtime uses the same Keychain service and account names across profiles, potentially shared with other applications using the Chromium defaults.

Changing a bundle identifier, product name, or profile directory does not change those hardcoded Keychain names. The installed CEF headers and Rust bindings have no supported setting for app-specific names. [CEF 152 settings](https://github.com/chromiumembedded/cef/blob/708dc140cbc3286826a8abef89dc23a44ff9ea72/include/internal/cef_types.h)

## Required fix

[CEF PR #4247](https://github.com/chromiumembedded/cef/pull/4247), merged on September 9, 2026, adds `keychain_service_name` and `keychain_account_name` to CEF settings. It is absent from the runtime bundled here.

Mapbrowser needs a CEF build containing that change, plus matching Rust bindings and API selection. A custom CEF build with the upstream change is required until a suitable binary release is available. Configure the names before initialization as `Mapbrowser Safe Storage` and `Mapbrowser`. Use a fresh test profile when validating the change, since data encrypted with the previous key may become unreadable.

Suppressing the dialog by using a mock key, disabling encryption, or changing another application's Keychain item is not a fix. This prototype does not use those workarounds.
