# Third-Party Notices

This file records source-code components incorporated into this repository and
the license selected for their distribution here. The repository's own license
does not replace these notices.

## md5

- Files: `protocol/src/encoding/md5.rs`
- Source: <https://github.com/stainless-steel/md5>
- Upstream license: Apache-2.0 OR MIT
- License selected for this distribution: MIT
- Copyright: 2015-2026 The md5 Developers

```text
Copyright 2015-2026 The md5 Developers

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

## RustCrypto Blowfish

- Files: `protocol/src/encoding/blowfish.rs`, `protocol/src/encoding/blowfish_consts.rs`
- Source: <https://github.com/RustCrypto/block-ciphers/tree/master/blowfish>
- Modifications: adapted for this crate's API and protocol integration
- Upstream license: Apache-2.0 OR MIT
- License selected for this distribution: MIT
- Copyright: Copyright (c) 2016-2024 The RustCrypto Project Developers

```text
Copyright (c) 2016-2024 The RustCrypto Project Developers

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
```

## Twofish

- File: `protocol/src/encoding/twofish.rs`
- Copyright: Copyright (c) 2017 Alexander Krotov
- License: MIT

The complete MIT license text and copyright notice are retained in the source
file as required by the license.

## Linked dependencies under non-permissive licenses

The components above are source code incorporated directly into this
repository. In addition, some workspace crates link against third-party
dependencies (via Cargo, unmodified) whose license is not MIT or
Apache-2.0. These are not copied or modified here; they are only listed for
transparency.

- **`colored`** (used by `examples/common`, directly and transitively through
  `fern`'s optional `colored` feature) is licensed under the
  [Mozilla Public License 2.0](https://www.mozilla.org/en-US/MPL/2.0/)
  (MPL-2.0). MPL-2.0 is a file-level weak-copyleft license: it does not
  require relicensing of this project's own code, but any modifications to
  `colored`'s own source files would need to remain under MPL-2.0. This
  repository does not modify `colored`'s source.
