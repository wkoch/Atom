// Extensão mínima de teste sobre o SDK: devolve `params.params` em `service.invoke`.
use atom_sdk::{Client, Declare, Invoke, ProvidesEntry};

struct Echo;

impl atom_sdk::Handler for Echo {
    fn declare(&self) -> Declare {
        Declare {
            provides: vec![ProvidesEntry {
                contract: "test.echo@0".to_string(),
                version: "0.1.0".to_string(),
            }],
            ..Default::default()
        }
    }

    async fn on_invoke(
        &self,
        _client: &Client,
        inv: Invoke,
    ) -> Result<serde_json::Value, atom_sdk::RpcError> {
        Ok(inv.params)
    }
}

#[tokio::main]
async fn main() {
    atom_sdk::run(Echo).await;
}
