# Apple Passwords and Touch ID

The requested provider is Apple Passwords / iCloud Keychain. Touch ID authorizes a passkey operation on the Mac. Chromium's older, browser-local Touch ID credential store is a separate implementation and cannot substitute for passkeys already saved in Apple Passwords.

## Current status

The normal `make run` build uses an ad hoc signature and cannot access Apple Passwords passkeys. Adding a Bluetooth purpose string fixed the earlier macOS privacy crash; it did not enable this provider.

Chromium 152 checks for `com.apple.developer.web-browser.public-key-credential` before making its Apple Passwords integration available. Apple manages this capability. The organization Account Holder must request approval for a macOS browser, then generate a matching provisioning profile. A development signing certificate alone does not grant access. [Apple entitlement requirements](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.web-browser.public-key-credential), [Chromium entitlement check](https://github.com/chromium/chromium/blob/152.0.7977.83/device/fido/mac/icloud_keychain_sys.mm)

There is also a native-window integration issue. Chromium normally obtains the passkey dialog's presentation window through its Views UI. Mapbrowser embeds an Alloy-style CEF view in a GPUI window. The prepared [Chromium patch](../patches/cef-152-native-passkeys.patch) adds a fallback to the web contents' actual native window. This patch must be included in a custom CEF build; it does not change the prebuilt framework downloaded by Cargo. Source patch applicability is not a compiled or runtime verification.

Existing Apple Passwords passkeys have not been tested in Mapbrowser. Completing this feature requires both the approved signing configuration and a CEF runtime with the window fix, followed by real registration and authentication tests.

Development verification on September 12, 2026: `make check` passed; `make test` passed 15 Rust tests and 15 Python signing tests. Both the default and isolated QA release bundles built and passed signature verification. The generated ad hoc bundle has HTTP/S registration on the main app only and contains no restricted passkey entitlement or embedded profile. Provisioned signing tests use synthetic profiles and mocked system commands; they do not establish that macOS will accept a real profile.

An isolated native-app check delivered a cold-launch HTTP URL and a second URL while running. Both appeared as separate saved tabs and produced successful local-server requests; the second page also executed its heartbeat script. Computer-use screenshots showed the tab titles and addresses but an empty page area, so visual page rendering was not confirmed in this check. The QA app and server were closed. This was a URL-routing check, not a passkey test.

## Signing an approved build

Use a provisioning profile issued for the explicit app identifier `dev.mapbrowser.browser`, with the browser passkey entitlement enabled, and a signing certificate included in that profile. The bundle script accepts an exact installed identity name or its complete SHA-1 fingerprint:

```sh
python3 scripts/bundle.py --release \
  --sign-identity 'YOUR SIGNING IDENTITY' \
  --provisioning-profile '/absolute/path/Mapbrowser.provisionprofile'
```

The script checks the profile and identity before building. It embeds the profile and adds the browser entitlement to the main application. Helper processes keep their Chromium runtime entitlements. Builds with neither signing argument use the normal ad hoc configuration and remove any previously embedded profile. Supplying only one signing argument is an error.

Mapbrowser declares HTTP and HTTPS URL handling and opens external web links in new tabs in the active space, as required for a browser. It does not change the system's default browser.

## Native CEF integration

The patch targets Chromium `152.0.7977.83`, the source version corresponding to the currently bundled runtime. Apply it to the Chromium source checkout used for a custom arm64 CEF build after CEF's own patches. Verify compatibility with the project's pinned Rust CEF bindings and CEF API version before replacing the runtime. A different Chromium version needs a reviewed patch and matching bindings.

Do not add Chromium's compiled `.org.chromium.Chromium.webauthn` access group to this app to suppress the local Touch ID warning. That does not grant access to Apple Passwords. The browser-local Touch ID authenticator would need its own Mapbrowser-scoped configuration and signing work. [Chromium local Touch ID configuration](https://github.com/chromium/chromium/blob/152.0.7977.83/chrome/browser/webauthn/chrome_web_authentication_delegate.cc)

## Acceptance checks

After the approved, patched build is available:

1. Verify the signed main application has the browser passkey entitlement and matching embedded provisioning profile. Verify helper signatures and sandbox operation.
2. Test a WebAuthn registration and authentication with a disposable test account. Check that the system offers Apple Passwords, that its sheet belongs to the GPUI window, and that cancellation leaves the app usable. A virtual authenticator does not verify Touch ID integration.
3. Have the user authorize Apple Passwords access and complete Touch ID on the Mac. Verify an existing Google passkey can complete authentication.
4. Repeat in a different space. Apple Passwords is a system provider shared across spaces; website cookies and signed-in sessions remain separated by workspace profiles.

The existing Chromium Safe Storage limitation affects persisted browser sessions independently. See [Keychain storage](keychain.md).
