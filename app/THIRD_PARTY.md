# Third-party components in the desktop app (ShredLab)

Bundled with the app UI (`app/src`). Audio components are listed in
[`crates/stemx-audio/THIRD_PARTY.md`](../crates/stemx-audio/THIRD_PARTY.md);
model weights in [`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md).

## Fonts

All fonts are bundled as woff2 in `src/assets/fonts/` and loaded locally (no
network requests). They were subset with fonttools (`pyftsubset`) from the
official Google Fonts sources.

| Font | Use | Copyright | License | Subset | Source |
|---|---|---|---|---|---|
| Rock Salt 1.001 | Wordmark letters | Copyright (c) 2010 by Font Diner, Inc DBA Sideshow | Apache-2.0 ([text](src/assets/fonts/LICENSE-Apache-2.0.txt)) | Latin (U+0020–007E, U+00A0–00FF, dashes, quotes, ellipsis) | [google/fonts `apache/rocksalt`](https://github.com/google/fonts/tree/main/apache/rocksalt) |
| Permanent Marker 1.001 | Stem labels ("scribble strips") | Copyright (c) 2010 by Font Diner, Inc. | Apache-2.0 ([text](src/assets/fonts/LICENSE-Apache-2.0.txt)) | Latin (as above) | [google/fonts `apache/permanentmarker`](https://github.com/google/fonts/tree/main/apache/permanentmarker) |
| Noto Music 2.004 | Treble and bass clefs in the wordmark and the app icon | Copyright 2022 The Noto Project Authors | SIL OFL 1.1 ([text](src/assets/fonts/OFL-NotoMusic.txt)) | U+1D11E 𝄞 and U+1D122 𝄢 only | [google/fonts `ofl/notomusic`](https://github.com/google/fonts/tree/main/ofl/notomusic) (upstream [notofonts/music](https://github.com/notofonts/music)) |

The app icon (`src-tauri/icons/app-icon.svg`, made by
[`tools/make_icon.py`](../tools/make_icon.py)) contains the two Noto Music
clef outlines converted to paths; it is listed here for attribution.
