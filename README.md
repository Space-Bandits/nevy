# 🎮 Nevy

> Fast, reliable networking for Bevy built on modern transport protocols! 🚀

## What is Nevy? 🤔

Nevy is a networking library for the [Bevy game engine](https://bevyengine.org/) that provides high-performance, easy-to-use networking capabilities using modern transport protocols like QUIC and WebTransport. It's designed to make multiplayer game development in Bevy a breeze! 🌊

### Key Features 🌟

- 🔒 Secure by default with TLS 1.3
- 🌐 Cross-platform support (Desktop & Web)
- 📦 Multiple transport protocols:
  - ⚡ QUIC for native platforms
  - 🌍 WebTransport for browser support
- 🎯 Easy-to-use Bevy plugin interface
- 📬 Message serialization & deserialization
- 🔄 Stream-based communication
- 🎨 Clean, modular architecture

## Quick Start 🚀

1. Add Nevy to your `Cargo.toml`:
```toml
[dependencies]
nevy = "0.1.0"
```

2. Create a simple client:
```rust
use bevy::prelude::*;
use nevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(MinimalPlugins)
        .add_plugins(EndpointPlugin::default())
        .add_plugins(StreamHeaderPlugin::default())
        .add_systems(Startup, setup_client)
        .run();
}

fn setup_client(mut commands: Commands) {
    // Create client endpoint...
}
```

## Examples 📚

Check out our examples to get started:
- 🎮 Simple client/server
- 📨 Message passing
- 🌊 Stream handling
- 🌐 Web client

## Protocol Support 🔌

### QUIC ⚡
Native platform transport using QUIC protocol for:
- Reliable ordered delivery
- Stream multiplexing
- Connection migration
- Low latency

### WebTransport 🌐
Browser support using WebTransport for:
- Web client connectivity
- Browser game development
- Cross-platform play

## Architecture 🏗️

Nevy is built with modularity in mind:
```
nevy/
├── 🔧 transport_interface/    # Core transport traits
├── 🎮 bevy_interface/        # Bevy integration
├── 📦 nevy_messaging/        # Message handling
├── ⚡ nevy_quic/            # QUIC implementation
├── 🌐 nevy_web_transport/   # WebTransport implementation
└── 🧩 nevy_wasm/           # WASM support
```

## Contributing 🤝

We love contributions! Whether it's:
- 🐛 Bug reports
- 💡 Feature requests
- 📝 Documentation improvements
- 🔧 Code contributions

Please feel free to:
1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Submit a pull request

## License 📄

This project is licensed under [INSERT LICENSE] - see the LICENSE file for details.

## Acknowledgments 🙏

- The amazing Bevy community
- Contributors to the QUIC and WebTransport protocols
- All our wonderful contributors!

---

Made with ❤️ for the Bevy community
