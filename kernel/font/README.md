# kernel/font

`Lat15-Terminus16.psf` — an 8x16 PSF1 console font in ISO-8859-15 order, embedded
into the kernel image by `kernel/src/fbcon.rs`.

**Licence: SIL Open Font License 1.1** (`LICENSES/OFL-1.1.txt`), *not* the project's
GPL-2.0-or-later. It is the *Terminus Font* by Dimitar Toshkov Zhekov
(Copyright (c) 2010, Reserved Font Name "Terminus Font"), converted to PSF1 and
reordered to Latin-15 by Debian's `console-setup` package (`Lat15-Terminus16.psf.gz`).
The file is used unmodified and is not sold by itself, which is what the OFL asks.

Note on an earlier version of this file: it said the font was "public domain" because
`console-setup`'s copyright file calls the *converted console fonts* "public domain by
nature". That sentence only covers Debian's own conversion work; the glyph designs
come from upstream Terminus, which is OFL-1.1 (see `/usr/share/doc/console-setup/copyright.fonts.gz`).
