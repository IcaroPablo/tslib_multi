# tslib

Bibliothèque modulaire et cross-platform pour créer des clients TeamSpeak 3/6 et des bots en Rust, avec bindings Python, Java et C.

## Modules

| Module | Description |
|--------|-------------|
| `tslib-core` | Connexion, identité, protocole, gestion de fichiers, permissions |
| `tslib-audio` | Capture/lecture audio, codec Opus |
| `tslib-chat` | Messages texte, parsing BBCode complet |
| `tslib-channel` | Gestion des channels, arborescence |
| `tslib-bot` | Framework bot avec commandes et plugins |
| `tslib-ffi` | Bindings C (bibliothèque partagée/statique) |
| `tslib-python` | Bindings Python natifs (PyO3) |
| `tslib-jni` | Bindings Java/Android natifs (JNI) |

## Fonctionnalités

- Connexion aux serveurs TeamSpeak 3 et 6
- Gestion des identités ECDH (création, import/export, niveau de sécurité)
- Système d'événements complet (35+ types)
- Audio temps réel (Opus voice/music, capture/playback via cpal)
- Chat avec parsing BBCode (tags imbriqués, couleurs, URLs, images)
- Arborescence des channels et gestion des utilisateurs
- Gestionnaire de fichiers (listing, upload, download, suppression, renommage, création de dossiers)
- Permissions de channels (query et hints)
- Icône serveur (récupération de l'icon_id)
- Framework bot avec préfixes de commandes
- Bindings natifs : C (FFI), Python (PyO3), Java/Android (JNI)
- 242 tests unitaires et d'intégration

## Compilation

### Prérequis

- Rust 1.75+
- Pour l'audio : ALSA (Linux), CoreAudio (macOS), WASAPI (Windows)

### Build

```bash
git clone --recursive https://github.com/flamme-demon/tslib_multi.git
cd tslib_multi

# Workspace complet
cargo build --release

# Un crate spécifique
cargo build --release -p tslib-core
```

### Build Android (JNI)

Nécessite [cargo-ndk](https://github.com/nickelc/cargo-ndk) et le NDK Android.

```bash
# Via le script fourni
./build_android.sh

# Ou manuellement
ANDROID_NDK=~/Android/Sdk/ndk/27.2.12479018 \
cargo ndk -t arm64-v8a -t x86_64 \
  -o ../TS6_Droid/app/src/main/jniLibs \
  build --release -p tslib-jni
```

### Exemples

```bash
# Bot simple
cargo run -p simple-bot -- --server localhost --nickname MyBot

# Client voix
cargo run -p voice-client -- --server localhost --nickname MyClient

# Tests offline
cargo run -p test-suite -- --offline

# Tests complets (serveur TS3 requis)
cargo run -p test-suite -- --server <host:port> --password <password>
```

### Documentation

```bash
cargo doc --workspace --open
```

## Exemples d'utilisation

### Rust — Bot

```rust
use tslib_bot::{Bot, BotConfig};
use tslib_core::Identity;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity = Identity::create()?;
    let config = BotConfig::builder()
        .address("localhost:9987")
        .identity(identity)
        .nickname("MonBot")
        .command_prefix("!")
        .build()?;

    let mut bot = Bot::new(config).await?;
    bot.command("ping", |ctx| async move {
        ctx.reply("Pong!").await
    }).await;

    bot.run().await
}
```

### Rust — Client voix

```rust
use tslib_core::{Client, ClientConfig, Identity};
use tslib_audio::{AudioManager, AudioConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity = Identity::create()?;
    let config = ClientConfig::builder()
        .address("localhost:9987")
        .identity(identity)
        .nickname("MonClient")
        .build()?;

    let client = Client::connect(config).await?;
    let audio = AudioManager::new(AudioConfig::default())?;
    audio.start_capture().await?;
    audio.start_playback().await?;

    // Boucle d'événements...

    client.disconnect().await
}
```

### Python (PyO3)

```bash
pip install maturin
maturin develop -m crates/tslib-python/Cargo.toml
```

```python
import tslib

identity = tslib.Identity()
client = tslib.Client("localhost:9987", identity, "PyBot")
client.wait_connected()

for user in client.users():
    print(f"  {user.nickname} (channel {user.channel_id})")

client.send_server_message("Hello from Python!")
client.disconnect()
```

### Java (JNI)

```bash
cargo build --release -p tslib-jni
javac java/src/main/java/dev/tslib/*.java
```

```java
import dev.tslib.*;

public class Example {
    static { System.loadLibrary("tslib_jni"); }

    public static void main(String[] args) {
        try (Identity identity = new Identity()) {
            try (Client client = new Client("localhost:9987", identity, "JavaBot")) {
                client.waitConnected();
                client.sendServerMessage("Hello from Java!");

                for (User user : client.getUsers()) {
                    System.out.println(user.nickname);
                }
            }
        }
    }
}
```

### C (FFI)

```bash
cargo build --release -p tslib-ffi
# Header généré : crates/tslib-ffi/include/tslib.h
```

```c
#include "tslib.h"

int main() {
    tslib_init();
    TsLibIdentity* identity = tslib_identity_create();
    TsLibClient* client = tslib_client_connect(
        "localhost:9987", identity, "CBot", NULL
    );
    if (client) {
        tslib_client_send_server_message(client, "Hello from C!");
        tslib_client_free(client);
    }
    tslib_identity_free(identity);
    tslib_shutdown();
    return 0;
}
```

## Architecture

```
tslib/
├── crates/
│   ├── tslib-core/       # Connexion, protocole, fichiers, permissions
│   ├── tslib-audio/      # Audio capture/playback, Opus
│   ├── tslib-chat/       # Messages texte, BBCode
│   ├── tslib-channel/    # Channels, arborescence
│   ├── tslib-bot/        # Bot framework, commandes
│   ├── tslib-ffi/        # Bindings C
│   ├── tslib-python/     # Bindings Python (PyO3)
│   └── tslib-jni/        # Bindings Java/Android (JNI)
├── java/                 # Classes Java (13 fichiers)
├── examples/
│   ├── simple-bot/
│   ├── voice-client/
│   ├── send-msg/
│   └── test-suite/       # Tests d'intégration
├── build_android.sh      # Script cross-compilation Android
└── Cargo.toml            # Workspace
```

## Licence

MIT OR Apache-2.0

## Crédits

- [tsclientlib](https://github.com/ReSpeak/tsclientlib) — Implémentation du protocole TeamSpeak
- [opus](https://opus-codec.org/) — Codec audio
- [cpal](https://github.com/RustAudio/cpal) — Audio cross-platform
- [PyO3](https://pyo3.rs/) — Bindings Python
