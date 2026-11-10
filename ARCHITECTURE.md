# nuggtbot

A from-scratch, headless, Rust re-implementation of a Streamer.bot-style stream automation bot, configured from any browser over the network.

Goal: low RAM, single static binary, no GUI, no Wine. Everything Streamer.bot does, reached through a web UI + WebSocket API instead of a desktop window.

> Reality check: Streamer.bot is a mature app with hundreds of features built over years. Full parity is a long, phased roadmap, not one build. This document is the target map; the code grows one vertical slice at a time. The first running slice (the MVP below) is deliberately small and complete, not a stub of 100 half-features.

## Function catalog (observed in the running Streamer.bot GUI)

- **Platforms** — connect accounts and receive events: Twitch, YouTube, Kick, Trovo. Events: chat message, follow, sub/resub/gift, cheer/bits, raid, channel-point reward, host, ban/timeout, stream online/offline.
- **Chat** — read and send chat messages; live chat feed; per-platform.
- **Commands** — custom chat commands (trigger text → Action), system commands, cooldowns (global/user), permission by role (broadcaster/mod/vip/sub/viewer), aliases, cost.
- **Actions & Queues** — an **Action** is an ordered list of **sub-actions**. **Queues** serialize Actions so they do not overlap.
- **Sub-actions** (the engine's breadth): send chat message, delay/wait, set/get/increment **variable**, conditional/logic + branch, random, loop, HTTP request, OBS (scene/source/filter/record/stream), VTube Studio (hotkey/model/expression), play sound / TTS, read/write file, run embedded code, trigger another Action, set argument.
- **Triggers / Events** — what starts an Action: command, chat message, channel-point reward, follow/sub/cheer/raid, **Timed Action** (interval), file/folder watcher, file tail, MIDI, hotkey, WebSocket message, HTTP request, manual.
- **Timed Actions (Timers)** — run an Action every N seconds / lines of chat.
- **Variables** — global and per-user, persistent, typed-ish (string/number/json).
- **Viewers** — viewer list, roles, currency/points, watch time, first-words.
- **Integrations** — OBS, VTube Studio, Throwing System, Streamlabs, StreamElements, Elgato (Stream Deck / Camera / Wave), HypeRate/Pulsoid (heart rate), Lumia Stream, Pally.gg, Crowd Control, DonorDrive, StreamLoots, Streamfog, TipeeeStream, TreatStream, VoiceMod, Meld Studio, PolyPop, Mtion, Pulsoid, Speaker.bot, and more.
- **Servers/Clients** — built-in **WebSocket Server** (docks/overlays connect in), **WebSocket Clients** (connect out), **HTTP Server**.
- **Overlays / Docks** — browser sources (e.g. chat/event feed) that connect to the WebSocket Server.
- **Services** — File/Folder Watcher, Timers, File Tails.
- **Other** — MIDI in/out, Voice Control, Global Variables view, Event Feed, Import/Export of Actions, Settings.

## Target architecture (Rust)

```
             ┌──────────────── axum (HTTP + WebSocket) ────────────────┐
browser ───► │  web config UI (SPA)  ·  REST API  ·  /ws event stream  │  ◄── docks/overlays
             └───────────────────────────┬─────────────────────────────┘
                                          │
        ┌──────────────── core event bus (tokio broadcast) ────────────────┐
        │   Event {platform, kind, payload}   →   Trigger match             │
        └───────────────┬───────────────────────────────┬──────────────────┘
                        │                                │
                 Action engine                     Integrations (modules)
          (run sub-actions in order,          Twitch / YouTube / Kick /
           queues, variables, conditions)     OBS / VTS / … each a task
                        │
                   Store (persistence: commands, actions, variables, settings)
```

- **Runtime:** `tokio` async, single process, headless daemon.
- **Event bus:** `tokio::sync::broadcast` — integrations publish `Event`s; the engine subscribes and matches triggers.
- **Engine:** executes an Action = `Vec<SubAction>` sequentially; supports queues, variables, conditions, delays.
- **Integrations:** one module/task each; Twitch first (IRC/EventSub), then OBS (obs-websocket client), then the rest.
- **Web:** `axum` serves the config SPA, a REST API for CRUD (commands/actions/variables/settings), and a WebSocket server (the Streamer.bot-equivalent for docks + a live event/chat feed).
- **Persistence:** start with a JSON file store (`serde_json`); migrate to SQLite when the data model stabilizes. `shortcut: JSON store, move to SQLite once Actions/Variables schema settles`.
- **Config:** `config.toml` — bind address (default a Tailscale/LAN-reachable addr), ports, data dir.
- **Headless + network:** no desktop; bind the web UI + WS to the network so any browser configures it. Put it behind Tailscale like the rest of this host.

## Roadmap (phases)

1. **MVP core (this slice):** daemon + axum web UI + JSON store + WebSocket event feed + **Commands** + **Twitch chat** (read a channel, match commands, send responses when a bot token is set). Compiles and runs headless; configure in a browser.
2. Action engine with sub-actions (send message, variable set/get, delay, condition) + Timed Actions.
3. Twitch EventSub (follow/sub/cheer/raid/reward) + send via Helix/IRC with the user's token.
4. WebSocket Server compatible with overlays/docks; a chat/event-feed overlay.
5. OBS integration (obs-websocket v5 client) + VTube Studio.
6. Variables (global/user) + Viewers/currency + cooldowns/permissions.
7. More platforms (YouTube, Kick) and integrations, Import/Export, MIDI, file watchers.

## Non-goals (for now)

- Running Streamer.bot's C# custom scripts (different runtime). A later "run code" sub-action can embed a scripting language (e.g. Rhai/Lua) instead.
- 1:1 import of Streamer.bot export files (different schema); may add a converter later.

---

## Português (Brasil)

# nuggtbot

Uma reimplementação do zero, headless, em Rust, de um bot de automação de stream no estilo Streamer.bot, configurado a partir de qualquer navegador pela rede.

Objetivo: baixo consumo de RAM, binário estático único, sem GUI, sem Wine. Tudo o que o Streamer.bot faz, acessível por uma UI web + API WebSocket em vez de uma janela de desktop.

> Dose de realidade: o Streamer.bot é um aplicativo maduro, com centenas de recursos construídos ao longo de anos. Paridade completa é um roteiro longo e dividido em fases, não uma única build. Este documento é o mapa do alvo; o código cresce uma fatia vertical de cada vez. A primeira fatia funcional (o MVP abaixo) é deliberadamente pequena e completa, não um esboço de 100 recursos pela metade.

## Catálogo de funções (observado na GUI do Streamer.bot em execução)

- **Platforms** — conectar contas e receber eventos: Twitch, YouTube, Kick, Trovo. Eventos: mensagem de chat, follow, sub/resub/gift, cheer/bits, raid, channel-point reward, host, ban/timeout, stream online/offline.
- **Chat** — ler e enviar mensagens de chat; feed de chat ao vivo; por plataforma.
- **Commands** — comandos de chat personalizados (texto de gatilho → Action), comandos de sistema, cooldowns (global/por usuário), permissão por papel (broadcaster/mod/vip/sub/viewer), aliases, custo.
- **Actions & Queues** — uma **Action** é uma lista ordenada de **sub-actions**. **Queues** serializam Actions para que não se sobreponham.
- **Sub-actions** (a abrangência do motor): enviar mensagem de chat, delay/wait, set/get/increment de **variable**, condicional/lógica + branch, random, loop, requisição HTTP, OBS (scene/source/filter/record/stream), VTube Studio (hotkey/model/expression), tocar som / TTS, ler/gravar arquivo, executar código embutido, disparar outra Action, definir argumento.
- **Triggers / Events** — o que inicia uma Action: command, mensagem de chat, channel-point reward, follow/sub/cheer/raid, **Timed Action** (intervalo), file/folder watcher, file tail, MIDI, hotkey, mensagem WebSocket, requisição HTTP, manual.
- **Timed Actions (Timers)** — executar uma Action a cada N segundos / linhas de chat.
- **Variables** — globais e por usuário, persistentes, com tipagem aproximada (string/number/json).
- **Viewers** — lista de viewers, papéis, moeda/pontos, tempo assistido, primeiras palavras.
- **Integrations** — OBS, VTube Studio, Throwing System, Streamlabs, StreamElements, Elgato (Stream Deck / Camera / Wave), HypeRate/Pulsoid (batimentos cardíacos), Lumia Stream, Pally.gg, Crowd Control, DonorDrive, StreamLoots, Streamfog, TipeeeStream, TreatStream, VoiceMod, Meld Studio, PolyPop, Mtion, Pulsoid, Speaker.bot, e mais.
- **Servers/Clients** — **WebSocket Server** embutido (docks/overlays conectam-se a ele), **WebSocket Clients** (conectam-se para fora), **HTTP Server**.
- **Overlays / Docks** — browser sources (por exemplo, feed de chat/eventos) que se conectam ao WebSocket Server.
- **Services** — File/Folder Watcher, Timers, File Tails.
- **Other** — MIDI entrada/saída, Voice Control, visão de Global Variables, Event Feed, Import/Export de Actions, Settings.

## Arquitetura-alvo (Rust)

```
             ┌──────────────── axum (HTTP + WebSocket) ────────────────┐
browser ───► │  web config UI (SPA)  ·  REST API  ·  /ws event stream  │  ◄── docks/overlays
             └───────────────────────────┬─────────────────────────────┘
                                          │
        ┌──────────────── core event bus (tokio broadcast) ────────────────┐
        │   Event {platform, kind, payload}   →   Trigger match             │
        └───────────────┬───────────────────────────────┬──────────────────┘
                        │                                │
                 Action engine                     Integrations (modules)
          (run sub-actions in order,          Twitch / YouTube / Kick /
           queues, variables, conditions)     OBS / VTS / … each a task
                        │
                   Store (persistence: commands, actions, variables, settings)
```

- **Runtime:** `tokio` assíncrono, processo único, daemon headless.
- **Event bus:** `tokio::sync::broadcast` — as integrações publicam `Event`s; o motor se inscreve e casa os triggers.
- **Motor:** executa uma Action = `Vec<SubAction>` sequencialmente; suporta queues, variables, condições e delays.
- **Integrations:** um módulo/task para cada; Twitch primeiro (IRC/EventSub), depois OBS (cliente obs-websocket), depois o resto.
- **Web:** `axum` serve o SPA de configuração, uma REST API para CRUD (commands/actions/variables/settings) e um servidor WebSocket (o equivalente ao do Streamer.bot para docks + um feed de eventos/chat ao vivo).
- **Persistência:** começar com um armazenamento em arquivo JSON (`serde_json`); migrar para SQLite quando o modelo de dados estabilizar. `shortcut: JSON store, move to SQLite once Actions/Variables schema settles`.
- **Config:** `config.toml` — endereço de bind (por padrão, um endereço alcançável via Tailscale/LAN), portas, diretório de dados.
- **Headless + rede:** sem desktop; faça o bind da UI web + WS à rede para que qualquer navegador a configure. Coloque atrás do Tailscale, como o resto deste host.

## Roteiro (fases)

1. **Núcleo do MVP (esta fatia):** daemon + UI web axum + armazenamento JSON + feed de eventos WebSocket + **Commands** + **Twitch chat** (ler um canal, casar comandos, enviar respostas quando um token de bot estiver definido). Compila e roda headless; configure no navegador.
2. Motor de Actions com sub-actions (enviar mensagem, variable set/get, delay, condição) + Timed Actions.
3. Twitch EventSub (follow/sub/cheer/raid/reward) + envio via Helix/IRC com o token do usuário.
4. WebSocket Server compatível com overlays/docks; um overlay de feed de chat/eventos.
5. Integração com OBS (cliente obs-websocket v5) + VTube Studio.
6. Variables (global/usuário) + Viewers/moeda + cooldowns/permissões.
7. Mais plataformas (YouTube, Kick) e integrações, Import/Export, MIDI, file watchers.

## Não-objetivos (por enquanto)

- Executar os scripts personalizados em C# do Streamer.bot (runtime diferente). Uma sub-action de "run code" posterior pode embutir uma linguagem de script (por exemplo, Rhai/Lua) no lugar.
- Importação 1:1 dos arquivos de export do Streamer.bot (schema diferente); pode-se adicionar um conversor depois.
