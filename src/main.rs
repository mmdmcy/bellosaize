mod app;
mod persist;
mod project;

fn main() -> anyhow::Result<()> {
    app::run()
}
