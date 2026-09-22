//! Example Garnet mod: greets players, adds a `/hello` command, keeps a
//! join counter across restarts and censors one word.

use garnet_sdk::*;

struct Hello {
    joins: u64,
}

impl Mod for Hello {
    fn init(&mut self) {
        register_command("hello", "Say hello to everyone", None);
        if let QueryResult::Data { value: Some(v) } = ask(Query::LoadData { key: "joins".into() }) {
            self.joins = v.as_u64().unwrap_or(0);
        }
        info(&format!("hello mod ready, {} joins so far", self.joins));
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        match event {
            Event::PlayerJoin { uuid, name } => {
                self.joins += 1;
                perform(Action::StoreData {
                    key: "joins".into(),
                    value: self.joins.into(),
                });
                send_message(uuid, &format!("&aWelcome, {name}! &7You are join number {}.", self.joins));
                perform(Action::Title {
                    player: uuid,
                    title: "&cGarnet".into(),
                    subtitle: "&7have fun".into(),
                    fade_in: 10,
                    stay: 40,
                    fade_out: 10,
                });
                EventResult::default()
            }
            Event::Command { uuid, command, .. } if command == "hello" || command.starts_with("hello ") => {
                let who = uuid
                    .and_then(|u| players().into_iter().find(|p| p.uuid == u))
                    .map(|p| p.name)
                    .unwrap_or_else(|| "the console".into());
                broadcast(&format!("&d{who} says hello!"));
                EventResult::cancelled()
            }
            Event::Chat { message, .. } if message.to_ascii_lowercase().contains("creeper") => {
                EventResult::rewrite(message.replace("creeper", "c*****r").replace("Creeper", "C*****r"))
            }
            _ => EventResult::default(),
        }
    }
}

garnet_mod!(Hello { joins: 0 });
