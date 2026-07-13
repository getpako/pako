use std::io::{self, Write};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Output {
    json: bool,
}

impl Output {
    pub(crate) fn new(json: bool) -> Self {
        Self { json }
    }

    pub(crate) fn print(
        self,
        json: &serde_json::Value,
        human: impl std::fmt::Display,
    ) -> anyhow::Result<()> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(json)?);
        } else {
            println!("{human}");
        }
        Ok(())
    }

    pub(crate) fn is_json(self) -> bool {
        self.json
    }
}

pub(crate) fn confirm(prompt: &str) -> anyhow::Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
