# This crate's assets

`icon.ico` — the Linkr Bee mark, as the Windows executable wears it.

It is cut from `assets/linkr-bee-mark.svg` at the repository root: rasterize
that at 512×512 with alpha, then write the seven sizes Windows asks for
(16/24/32/48/64/128/256). Pillow does the second half in one call:

    python3 -c "from PIL import Image; im = Image.open('mark.png').convert('RGBA'); \
        im.resize((256, 256), Image.LANCZOS).save('icon.ico', \
        sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])"

`build.rs` embeds it on Windows targets only; see that file for why the check
is a target one rather than `#[cfg(windows)]`.
