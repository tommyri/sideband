# Development installer releases

Open [Release desktop installers](https://github.com/tommyri/sideband/actions/workflows/release-desktop.yml),
choose **Run workflow**, select `main`, enter a version (default `0.0.1`), and
check the operating systems to build. At least one is required. Nothing runs on
pushes, pull requests or a schedule. Only selected platform runners are started,
plus CI on the same commit and small preparation/publication jobs. Other refs are
refused, and the run waits for approval before building and before publishing; see
[release gates](release.md#release-gates).

| Selection | Architecture | Downloads |
| --- | --- | --- |
| Windows | x64 | NSIS setup `.exe` and WiX `.msi` |
| macOS | Apple Silicon / arm64 | `.dmg` and `.app.zip` |
| macOS Intel (opt-in) | x64 | `.dmg` and `.app.zip` |
| Linux | x64, Ubuntu 24.04 baseline | `.deb` and `.AppImage` |

Results appear under [GitHub Releases](https://github.com/tommyri/sideband/releases)
as a **prerelease** tagged `vVERSION`. The version must be three integers, e.g.
`0.0.1`; major/minor must be at most 255 and patch at most 654 to reserve MSI build numbers for alphas. This
version is embedded in the installer and About/build metadata using a Tauri
configuration override, so later releases do not require editing Cargo versions.
The release build uses the existing testing channel and guided test mechanisms.

For example, to publish only Windows from the CLI:

```sh
gh workflow run release-desktop.yml --ref main \
  -f version=0.0.1 -f updates=false -f windows=true -f macos=false -f linux=false
```

Matched Mac, Windows and server alphas build on dispatch. One approval publishes the installers, Docker image and dedicated signed update feeds. See [Publish a matched alpha](release.md#publish-a-matched-alpha).

## Reusing a version during development

Rerunning `0.0.1` replaces the selected platforms' assets in the existing
`v0.0.1` release. Unselected platforms retain their previous downloads. All
selected builds must succeed before publication starts, so a build failure leaves
the existing release untouched. Upload replacement itself is not atomic: if an
upload fails, rerun the workflow to repair the release.

The version tag moves to the latest publication commit. Different platforms can
therefore represent different commits after a partial rebuild; each platform's
`build-info.json` is the authoritative source identity. Download its matching
`SHA256SUMS` and `INSTALL.md` alongside the installer. This mutable-release policy
is temporary and unsuitable for stable production releases.

Quit the old app, re-download and reinstall to update a same-version build.
Signed auto-update builds can update to newer versions. The release form enables
this by default and requires the updater keypair; turn it off for manual test
installers. See [packaging and updater setup](release.md) for keys, optional OS
signing, platform feeds, and installed acceptance checks. Without OS credentials,
Windows packages remain unsigned and macOS packages ad-hoc signed.

## Comparing CI timings

The Windows x64 and Apple Silicon 0.1.1 release took
25m 33s from dispatch to publication and about 88 summed runner-minutes. The
Windows packaging and workspace CI jobs took 19m 08s and 18m 53s respectively.
Compare both job durations on a warm cache and after a `Cargo.lock` change before
tuning the Windows cache. The dispatch-to-publication time also includes reviewer
waits. [0.1.1 job and step timings](https://github.com/tommyri/sideband/actions/runs/45635515282)
