# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

> **Status.** No release is code-signed yet (v0.3.0 and v0.3.1 are not). Signing starts with the first
> release made after SignPath approves the project; until then this page describes how signing will
> work and nothing on the release page claims a signature that is not there.

This policy covers Moku, <https://github.com/dikaproject/MokuApp>, and the release files published
at <https://github.com/dikaproject/MokuApp/releases>.

## What is signed

Authenticode signatures are applied to:

- `moku.exe`, `moku-settings.exe` and `mokuctl.exe`, in the installer and in the portable zip, and
- `moku-setup.exe`, the installer (and uninstaller).

Nothing else is signed: the documents, `tools\claude-hook.ps1` and the checksum files are plain
files. Moku ships no third-party binaries; every library is compiled from source into these
programs. All of them carry the product name "Moku" and the same product version, which comes from
the one workspace version in `app/Cargo.toml`.

## Team roles

Moku is maintained by one person, who holds every role.

| Role | What it means | Member |
|---|---|---|
| Authors | Trusted to change the source code without additional review. | [dikaproject](https://github.com/dikaproject) |
| Reviewers | Review every change from a non-committer (a pull request) before it is merged. | [dikaproject](https://github.com/dikaproject) |
| Approvers | Approve every signing request. | [dikaproject](https://github.com/dikaproject) |

The maintainer uses multi-factor authentication for GitHub and for SignPath.

## How a release is signed

1. A release starts only when the maintainer pushes a version tag (`v*`) to the public repository.
2. The GitHub Actions workflow [`release.yml`](.github/workflows/release.yml) runs on a GitHub-hosted
   runner, tests the code, and builds the programs from that tag's source, with `--locked`
   dependencies. Nothing is built or signed on a developer machine.
3. The workflow sends the unsigned programs to SignPath (first the three `.exe` files, then the
   installer built from the signed ones). SignPath checks that the request comes from this repository
   and workflow run.
4. **Every signing request is approved manually by the approver** in SignPath before anything is
   signed. A release waits until the request is approved. There is no automatic approval.
5. The signing key stays at SignPath; Moku never holds it. The signed files come back to the same
   workflow run, which publishes them as a GitHub Release with the notes from
   [`CHANGELOG.md`](CHANGELOG.md).

## Moku's own update signature

Next to the Authenticode signature, each release has `moku-setup.exe.sig` and
`moku-setup.exe.sha256`. These are not SignPath's: the `.sig` is Moku's own ECDSA P-256 signature of
the installer's SHA-256, made with a key held by the maintainer, and it is what the in-app updater
checks before it runs a downloaded installer. It is made **after** the installer is signed, over the
final file. Windows does not know this key; SmartScreen and file Properties look only at the
Authenticode signature.

## Privacy

Moku has no account, no server and no telemetry, and contacts a network service only for a feature
that is switched on. Each service, when it is contacted and what is sent is in the
[privacy policy](PRIVACY.md).

## System changes and uninstalling

The installer works for the current user only (no administrator rights) and says what it does:
it installs into `%LOCALAPPDATA%\Programs\Moku`, adds a Start Menu shortcut and an Apps & Features
entry, and adds a start-up entry only if you choose it. It has a matching uninstaller (Apps &
Features, or `moku-setup.exe --uninstall`) that removes all of that and asks before deleting your
data.

## Reporting a problem

If a signed Moku file behaves in a way this policy does not allow, open an issue at
<https://github.com/dikaproject/MokuApp/issues>.
