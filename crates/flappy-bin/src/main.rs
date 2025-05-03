use anyhow::Result;
use clap::Parser;
use std::sync::Arc;

#[derive(Parser)]
struct Cli {
    #[clap(long)]
    mock: bool,

    /// OpenAI API key (read from CLI or OPENAI_API_KEY env)
    #[clap(long, env = "OPENAI_API_KEY")]
    openai_key: Option<String>,

    /// Model name (only for openai mode)
    #[clap(long, default_value = "gpt-3.5-turbo")]
    model: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let model_name = cli.model.clone();
        let llm: Arc<dyn flappy_core::LlmProvider> = if cli.mock {
            Arc::new(flappy_llm::mock::Mock::default())
        } else {
            #[cfg(feature = "openai")]
            {
                let key = cli.openai_key.or_else(|| std::env::var("OPENAI_API_KEY").ok()).expect("OpenAI key required");
                Arc::new(flappy_llm::openai::OpenAi::new(key, model_name.clone())?)
            }

            #[cfg(not(feature = "openai"))]
            {
                eprintln!("binary built without --features openai, falling back to mock");
                Arc::new(flappy_llm::mock::Mock::default())
            }
        };

        let app = flappy_core::App::new(llm, model_name);
        flappy_tui::run(app).await
    })
} 