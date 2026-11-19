# Attribution

This workspace is a clean-room Rust rewrite of the author's own Kotlin app (`baudet`), so its
protocol vocabulary and behaviour come from that source, not from a third party.

Two ideas here are borrowed from the ArtCraft apps rather than invented, and both are Apache-2.0:

* **The toolkit choice and its feature shape.** `eframe`/`egui` 0.36 with eframe's default
  features switched off, `default_fonts` + a software rasteriser, `egui_extras` for SVG, and
  `egui_kittest` as the way to test a GUI without a human at the window, follow
  [`storytold/craft-anything`](https://github.com/storytold/craft-anything) `crates/ui-egui`
  (Apache-2.0, see its `LICENSE-APACHE`). `rust-toolchain.toml` pins the `rust-version` that repo
  declares, because that is what makes their UI patterns portable here.
* **Rasterising SVG icons at the exact on-screen size** with `resvg` instead of shipping one PNG
  per dpi, likewise theirs.

No source file from ArtCraft is copied into this workspace. If a widget is later lifted rather than
re-implemented, it gets a per-file notice here and its licence header stays intact.
