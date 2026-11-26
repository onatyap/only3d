# Packaging

`.github/workflows/release.yml` builds every package. Pushing a `v<version>` tag publishes them as
a pre-release; running the workflow by hand builds them without publishing.

- `macos/`: `package.sh` makes only3d.app and the .dmg; `keychain.sh` loads a signing
  certificate on CI.
- `windows/`: `only3d.nsi` is the installer; `sign.ps1` signs the executables.
- `linux/`: the AppImage, the desktop entry and the .tar.gz.
- `third_party_notices.py` writes THIRD-PARTY-NOTICES.txt, which ships in every package.
- `release-notes.md` is the release's text (`__VERSION__` is filled in).

## Signing

Builds are signed when these repository secrets (or, for SignPath, the token and variables) are set (Settings ▸ Secrets and variables ▸
Actions); without them they stay unsigned, as before. Once they're set, change the install
steps in `release-notes.md` and the README, which tell people how to open unsigned builds.

### macOS: Developer ID and notarization

Needs a paid Apple Developer Program membership.

1. In the developer account, create a **Developer ID Application** certificate. Open it in
   Keychain Access, select the certificate and its private key, and export them as a .p12 with
   a password.
2. Make an app-specific password for the Apple ID at account.apple.com (Sign-In and Security ▸
   App-Specific Passwords).
3. Add the secrets:

| Secret | Value |
|---|---|
| `MACOS_CERT_P12_BASE64` | `base64 -i certificate.p12 \| pbcopy` |
| `MACOS_CERT_PASSWORD` | the .p12's password |
| `NOTARY_APPLE_ID` | the Apple ID's email |
| `NOTARY_TEAM_ID` | the 10-character team ID (Membership details) |
| `NOTARY_PASSWORD` | the app-specific password |

The app is signed with the hardened runtime and a timestamp, the .dmg is signed, sent to Apple's
notary service, and the ticket stapled. With only the first two secrets the build is signed but
not notarized, and macOS still asks before opening it.

To try it on a Mac, run `packaging/macos/package.sh <binary> <version> <dir>` with
`MACOS_SIGN_IDENTITY` set to the identity's name (as `security find-identity -v -p codesigning`
shows it) and, for notarization, the three `NOTARY_*` variables.

### Windows: SignPath Foundation (free for open source)

only3d applied to [SignPath Foundation](https://signpath.org), which signs open-source releases
for free with its own certificate ([docs/code-signing.md](../docs/code-signing.md) is the policy
it asks for). Once the project is accepted:

1. In SignPath, add the predefined **GitHub.com** trusted build system to the organization and
   link it to the project. Use [windows/signpath.xml](windows/signpath.xml) as the artifact
   configuration.
2. Make an API token for a submitter, and add it as the `SIGNPATH_API_TOKEN` secret.
3. Add the repository variable `SIGNPATH_ORGANIZATION_ID` (Settings ▸ Secrets and variables ▸
   Actions ▸ Variables). `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG` default to
   `only3d` and `release-signing`; set them if SignPath names them otherwise.

With the token set, each Windows job sends two signing requests (the two exes, then the
installer built from them), so a release waits for four approvals in SignPath, each within an
hour. Then change the Windows install steps in `release-notes.md` and the README.

### Windows: Authenticode with your own certificate

Needs a code-signing certificate as a .pfx with its private key (an OV or EV certificate from a
certificate authority). Certificates issued since June 2023 keep the key on hardware or in a
cloud signing service, so they can't be exported as a .pfx. For those, replace the signtool call
in `windows/sign.ps1` with the provider's own signing step (for example Azure Trusted Signing).
SignPath's free plan for open-source projects needs the repository to be public.

| Secret | Value |
|---|---|
| `WINDOWS_CERT_PFX_BASE64` | `base64 -i certificate.pfx \| pbcopy` |
| `WINDOWS_CERT_PASSWORD` | the .pfx's password |

`only3d.exe`, `only3d-app.exe` and the installer are signed with SHA-256 and a DigiCert timestamp
(`WINDOWS_TIMESTAMP_URL` picks another). The uninstaller that the installer writes is not signed.
Running the workflow by hand with **test_signing** ticked signs the Windows builds with a
throwaway self-signed certificate, to try these steps without a real one.
