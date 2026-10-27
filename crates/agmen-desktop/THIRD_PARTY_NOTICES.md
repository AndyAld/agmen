# Third-party notices: agmen-desktop

agmen is MIT licensed. The desktop app bundles or links the works below, each under its own license. Every license text below is compiled into the app and shown in Settings → About; the files are in `assets/`.

## Bundled assets

| Work | Files | License | Copyright | License text |
|---|---|---|---|---|
| IBM Plex Sans | `assets/fonts/ibm-plex-sans/*.ttf` | SIL Open Font License 1.1 | 2017 IBM Corp., Reserved Font Name "Plex" | `assets/fonts/ibm-plex-sans/license.txt` |
| Lilex | `assets/fonts/lilex/*.ttf` | SIL Open Font License 1.1 | 2019 The Lilex Project Authors | `assets/fonts/lilex/OFL.txt` |
| Lucide icons v1.61.0 | `assets/icons/lucide/*.svg` | ISC; the icons Lucide took from Feather are MIT | Lucide Icons and Contributors; Feather: 2013-present Cole Bemis | `assets/icons/LICENSE-lucide.txt` |

## Crates from Zed Industries (git dependencies)

| Crates | Source and revision | License |
|---|---|---|
| gpui, gpui_apple, gpui_macos, gpui_macros, gpui_shared_string, gpui_util, collections, derive_refineable, refineable, http_client, scheduler, sum_tree, ztracing, ztracing_macro | github.com/zed-industries/zed at 08cbb88 | Apache-2.0 (`assets/licenses/zed-LICENSE-APACHE.txt`) |
| zed-font-kit 0.14.1-zed | github.com/zed-industries/font-kit at 94b0f28 | MIT OR Apache-2.0; agmen uses it under MIT (`assets/licenses/font-kit-LICENSE-MIT.txt`) |

Licenses come from each crate's Cargo metadata (`cargo metadata --locked`, 2026-10-26). Crates from crates.io are checked by `cargo deny check` against `deny.toml`; shipping their license texts with release builds is the release pipeline's job (not built yet).

## Not bundled

zed-scap (github.com/zed-industries/scap) is in Cargo.lock through gpui's screen-capture feature, which agmen does not enable, so it is not built (`cargo tree -i zed-scap --target all --all-features` prints nothing). Its LICENSE is not MIT despite its heading: it grants use only to people who outrank its author in Valorant. agmen must never link it.


Zed's own UI icons are never copied into agmen. File icons come at run time from the person's installed Zed icon theme or from the operating system, and themes from their installed Zed themes; agmen ships neither.
