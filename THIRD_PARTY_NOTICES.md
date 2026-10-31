# Third-party notices

Toolport includes selected brand icons only to identify compatible clients and
services. All product names, logos, and brands are property of their respective
owners. Their use does not imply endorsement.

## Lobe Icons

Selected client and server icons are adapted from
[`@lobehub/icons-static-svg`](https://github.com/lobehub/lobe-icons), licensed
under the MIT License.

Copyright (c) 2023 LobeHub

## Devicon

The Visual Studio Code client icon is adapted from
[Devicon](https://github.com/devicons/devicon), licensed under the MIT License.

Copyright (c) 2015 konpa

## Codewhale

The Codewhale client icon and native skill frontmatter reader are adapted from
[Codewhale](https://github.com/codewhale-hq/Codewhale), licensed under the MIT License.

Copyright (c) 2024-2025 DeepSeek-TUI Contributors

## OpenClaw

The OpenClaw client icon is adapted from the
[OpenClaw 2026.10.4 package](https://www.npmjs.com/package/openclaw/v/2026.10.4),
licensed under the MIT License.

Copyright (c) 2026 OpenClaw Foundation

## OpenHands

The OpenHands client icon is adapted from
[`src/assets/branding/openhands-logo.svg`](https://github.com/OpenHands/OpenHands/blob/main/src/assets/branding/openhands-logo.svg)
in OpenHands, licensed under the MIT License. The monochrome adaptation changes
white paths to black and removes the fixed dimensions.

Copyright © 2025 OpenHands contributors

## MIT License text

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

## Simple Icons

Selected client and server icon data is adapted from
[Simple Icons](https://github.com/simple-icons/simple-icons), whose collection
is released under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/).
Individual marks may remain subject to their owners' trademark and usage rules.
See the [Simple Icons disclaimer](https://github.com/simple-icons/simple-icons/blob/develop/DISCLAIMER.md).

## Lucide

The icons for the Fetch, Filesystem, Memory, Sequential Thinking and Time
reference servers are adapted from [Lucide](https://lucide.dev), licensed under
the ISC License.

Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as part of
Feather (MIT). All other copyright (c) for Lucide are held by Lucide
Contributors 2022.

Permission to use, copy, modify, and/or distribute this software for any purpose
with or without fee is hereby granted, provided that the above copyright notice
and this permission notice appear in all copies.

## Vendor marks

The Apify, Browserbase, Composio, Context7, DataForSEO, Keenable, Parallel and
Playwright marks are taken from each project's own website. They are used only
to identify the corresponding server and remain the property of their owners.

The agent-qa mark is taken from [vostride’s official brand asset](https://vostride.com/brand/vostride-colored-logo.svg).
It is used only to identify the corresponding server and remains the property of vostride.

## Client vendor marks

The Crush, Jan, Witsy and ZCode client icons are taken from each project's own
website or repository. They are used only to identify the corresponding client
and remain the property of their owners.

## RevenueCat

The RevenueCat logomark is vendored from the official
[RevenueCat press kit](https://www.revenuecat.com/press-kit). It is used only to
identify the corresponding server. The mark remains the property of RevenueCat.

## BGE search embeddings

The gateway bundles an f16 storage conversion of
[BAAI/bge-small-en-v1.5](https://huggingface.co/BAAI/bge-small-en-v1.5),
revision `5c38ec7c405ec4b44b94cc5a9bb96e735b38267a`, under the MIT License.
Copyright (c) 2022 staoxiao. The full notice is retained in
`src-tauri/assets/search/LICENSE` and in the gateway binary.
Toolport changes numeric storage only; no Toolport queries or labels train it.
Candle performs CPU inference with f32 arithmetic from these f16 stored weights.

The loader uses Hugging Face tokenizers 0.22.2 under Apache-2.0.
Its full license is retained in `src-tauri/assets/search/tokenizers-LICENSE`.

## nanobot client logo

The nanobot mark in `src/assets/client-logos/nanobot.svg` and
`src-tauri/icons/client-logos/nanobot.png` is the upstream WebUI brand icon from
[HKUDS/nanobot](https://github.com/HKUDS/nanobot/blob/b702ac6/webui/public/brand/nanobot_icon_192.png),
used under its MIT license.

```text
MIT License

Copyright (c) 2025-present Xubin Ren and the nanobot contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## PyYAML resolver compatibility

The YAML 1.1 implicit scalar patterns in
`src-tauri/src/skills/nanobot_metadata.rs` are derived from
[PyYAML's resolver](https://github.com/yaml/pyyaml/blob/6.0.3/lib/yaml/resolver.py)
to match nanobot's metadata interpretation. PyYAML is licensed under MIT:

```text
Copyright (c) 2017-2021 Ingy döt Net
Copyright (c) 2006-2016 Kirill Simonov

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
