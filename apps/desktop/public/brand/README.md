# Sideband identity

Selected on 2026-11-03: **Sideband**, with the **offset-bands** logo (concept 01).
Domain selection is deferred. The original naming research and concept boards
remain in [the exploration](../../design/sideband-exploration/README.md).

`sideband-mark.svg` is the editable vector master, redrawn from the selected
concept. Its two offset bands form an S. Teal is `#00b79c`; the app icon uses
warm white `#faf7f0` on charcoal `#24282b`. The SVGs have transparent outer areas.

`sideband-app.svg` and the native PNG/ICO/ICNS icons are generated. Regenerate
from the repository root after installing desktop dependencies:

```sh
node scripts/generate-brand-icons.mjs
```

The browser favicon and server rail use the vector mark. Packaged apps and the
existing tray use the native app icon. The monochrome `tray-icon.png` is also
provided for future template-icon integration.

The product name changes; `com.socialspeak.desktop`, Cargo/npm package names,
`ss-*` commands, protocol IDs, environment variables and existing storage keys
remain stable for compatibility. Existing server names are user data, including
the legacy database seed; immutable historical migrations are unchanged. New
server configuration templates use Sideband. Copyright attributions and old
implementation notes may retain the former name.
