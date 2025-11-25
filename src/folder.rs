 use serde::{Deserialize, Serialize};

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct Folder {
      pub path: String,
      pub name: String,
      pub depth: u32,
  }

  impl Folder {
      pub fn new(path: String) -> Self {
          let name = path
              .trim_end_matches('/')
              .rsplit('/')
              .next()
              .unwrap_or(&path)
              .to_string();

          let depth = path.matches('/').count() as u32;

          Self { path, name, depth }
      }
  }


