//! Build script: embed the nuggtbot logo as the Windows executable icon (shown in
//! Explorer/taskbar). Runs only when building on a Windows host (the CI Windows
//! runners); it rasterizes `assets/logo.svg` and encodes an `.ico` in memory, so
//! no prebuilt icon file is needed. On other hosts it does nothing.
fn main() {
    #[cfg(windows)]
    windows_icon();
}

#[cfg(windows)]
fn windows_icon() {
    use std::io::Write;

    println!("cargo:rerun-if-changed=assets/logo.svg");
    let svg = match std::fs::read_to_string("assets/logo.svg") {
        Ok(s) => s,
        Err(_) => return,
    };
    let tree = match resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()) {
        Ok(t) => t,
        Err(_) => return,
    };

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 32, 48, 64, 128, 256] {
        let mut pixmap = match resvg::tiny_skia::Pixmap::new(size, size) {
            Some(p) => p,
            None => continue,
        };
        let ts = tree.size();
        let scale = size as f32 / ts.width().max(ts.height());
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        let image = ico::IconImage::from_rgba_data(size, size, pixmap.data().to_vec());
        if let Ok(entry) = ico::IconDirEntry::encode(&image) {
            dir.add_entry(entry);
        }
    }

    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("nuggtbot.ico");
    let mut buf = Vec::new();
    if dir.write(&mut buf).is_err() {
        return;
    }
    if std::fs::File::create(&out).and_then(|mut f| f.write_all(&buf)).is_err() {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(out.to_str().unwrap());
    let _ = res.compile();
}
