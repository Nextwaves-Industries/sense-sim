//! `fetch <root> [folder]`: lists the upstream catalog, optionally downloads one asset folder.
#[path = "../src/remote.rs"]
mod remote;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("root"));
    let names = remote::list_assets()?;
    println!("{} assets upstream; first: {:?}", names.len(), &names[..3]);
    if let Some(folder) = args.next() {
        let p = std::sync::Arc::new(remote::Progress::default());
        let t = std::time::Instant::now();
        remote::download_folder(&root, &folder, p.clone());
        let mb = p.done.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6;
        println!("downloaded {mb:.1} MB in {:.1}s, error: {:?}", t.elapsed().as_secs_f32(), p.error.lock().unwrap());
    }
    Ok(())
}
