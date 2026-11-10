# nuggtbot plugins

A plugin is a native library (`.so` on Linux, `.dll` on Windows) that you
compile **outside** the bot and drop into the `plugins/` folder. The bot loads
plugins at boot. Adding or removing a plugin **never** touches or recompiles the
bot.

## How it works (summary)

- The bot is the main process. At startup it scans `plugins/` and loads every
  `*.so` / `*.dll` that exports the right ABI.
- The boundary between bot and plugin exchanges only **JSON inside a C-string**.
  Because of this the plugin **shares no Rust type with the bot** and can be
  compiled with any version of compiler and dependencies.
- The plugin shows up on its own in the panel, in the **Integrations** tab: it
  becomes a card (with a switch and a status badge) and its actions appear in the
  Actions editor.
- The plugin's config (host, token, etc.) lives in `plugins/<name>.config.json`,
  managed by the panel. The bot passes this config on every call.

## Building a plugin

1. Create a `cdylib` Rust crate. Use the `orbital-example/` example as a template.

   `Cargo.toml`:
   ```toml
   [lib]
   crate-type = ["cdylib"]

   [dependencies]
   plugin-sdk = { path = "CAMINHO/para/plugin-sdk" }
   serde_json = "1"
   # + o que seu plugin precisar (reqwest, etc.)
   ```

2. Implement three functions and call the macro:

   ```rust
   use serde_json::{json, Value};

   fn manifest() -> Value { json!({ "name": "hello", "card": { .. }, "actions": [ .. ] }) }
   fn run(ctx: Value) -> Value { json!({ "ok": true }) }
   fn status(cfg: Value) -> Value { json!({ "enabled": true, "reachable": true }) }

   plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status);
   ```

   The macro generates the five `extern "C"` functions (`plugin_abi_version`,
   `plugin_manifest`, `plugin_run`, `plugin_status`, `plugin_free`) and wraps
   `run`/`status`/`manifest` in `catch_unwind` — a `panic!` crossing the FFI is
   undefined behavior, so **always** use the macro.

3. Build and install:
   ```sh
   cargo build --release
   cp target/release/lib<nome>.so  CAMINHO/do/bot/plugins/<nome>.so
   ```
   In the panel, Integrations tab, click **Reiniciar bot**. The plugin appears.

## JSON contract

### manifest()  → the card and the actions
```json
{
  "name": "orbital",
  "card": {
    "title": "Orbital Live",
    "color": "#f0795e",
    "icon": "<circle .../><path .../>",   // conteúdo interno de um <svg> 24x24
    "fields": [
      { "key": "host", "label": "Host", "placeholder": "...", "kind": "text", "options": [] }
    ],
    "has_switch": true,    // mostra o liga/desliga (chave "enabled" na config)
    "has_status": true     // mostra o badge e chama status()
  },
  "actions": [
    {
      "name": "fire",
      "label": "orbital_fire",
      "doc": "texto de ajuda mostrado no editor",
      "fields": [
        { "key": "catastrophe", "label": "catastrophe", "kind": "select",
          "placeholder": "", "options": ["nuke","meteor"] }
      ]
    }
  ]
}
```
A field's `kind`: `"text"`, `"number"`, `"password"`, `"select"` (uses `options`), `"bool"`.

### run(ctx)  → runs an action
`ctx` received:
```json
{ "action": "fire", "user": "fulano",
  "params": { "catastrophe": "nuke" },
  "config": { "enabled": true, "host": "...", "port": 8899, "token": "" } }
```
Return `{"ok": true}` on success, or `{"ok": false, "error": "..."}` — the bot
logs the error. `params` are the action's fields (already with `%variáveis%`
resolved). `config` is the `plugins/<nome>.config.json`.

The call runs on a separate thread (`spawn_blocking`), so it can be blocking
(e.g. `reqwest::blocking`). The plugin owns its own concurrency.

### status(cfg)  → the card's badge
Receives the config, returns `{"enabled": bool, "reachable": bool}`. Keep it fast
(use a short timeout); it runs on every panel refresh.

## ABI and compatibility

`ABI_VERSION = 1`. If the bot ships a new version of the contract, it **refuses**
a `.so` of a different version (logs it and ignores it), instead of calling it
and risking UB. Recompile the plugin against the new `plugin-sdk` when this
happens.

## Security — read this

A `.so`/`.dll` is **native code with no sandbox**, running with the privileges of
the bot process (which runs as root). Loading a third-party plugin means running
arbitrary third-party code on your machine. **Only drop into `plugins/` a library
that you compiled yourself or trust completely.** The panel has no authentication
and is only safe behind Tailscale — do not expose it via Funnel.

A `panic!` crossing the FFI is UB and can take the bot down; the `export_plugin!`
macro guards against it with `catch_unwind`. Do not write the `extern "C"`
functions by hand without that protection.

---

## Português (Brasil)

# Plugins do nuggtbot

Um plugin é uma biblioteca nativa (`.so` no Linux, `.dll` no Windows) que você
compila **fora** do bot e larga na pasta `plugins/`. O bot carrega os plugins no
boot. Adicionar ou remover um plugin **nunca** mexe nem recompila o bot.

## Como funciona (resumo)

- O bot é o processo principal. No startup ele varre `plugins/` e carrega todo
  `*.so` / `*.dll` que exporta o ABI certo.
- A fronteira entre bot e plugin troca só **JSON dentro de uma C-string**. Por
  isso o plugin **não compartilha nenhum tipo Rust com o bot** e pode ser
  compilado com qualquer versão de compilador e de dependências.
- O plugin aparece sozinho no painel, na aba **Integrations**: vira um card
  (com switch e badge de status) e suas ações aparecem no editor de Actions.
- A config do plugin (host, token, etc.) fica em `plugins/<nome>.config.json`,
  gerenciada pelo painel. O bot passa essa config em toda chamada.

## Montando um plugin

1. Crie um crate Rust `cdylib`. Use o exemplo `orbital-example/` como molde.

   `Cargo.toml`:
   ```toml
   [lib]
   crate-type = ["cdylib"]

   [dependencies]
   plugin-sdk = { path = "CAMINHO/para/plugin-sdk" }
   serde_json = "1"
   # + o que seu plugin precisar (reqwest, etc.)
   ```

2. Implemente três funções e chame a macro:

   ```rust
   use serde_json::{json, Value};

   fn manifest() -> Value { json!({ "name": "hello", "card": { .. }, "actions": [ .. ] }) }
   fn run(ctx: Value) -> Value { json!({ "ok": true }) }
   fn status(cfg: Value) -> Value { json!({ "enabled": true, "reachable": true }) }

   plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status);
   ```

   A macro gera as cinco funções `extern "C"` (`plugin_abi_version`,
   `plugin_manifest`, `plugin_run`, `plugin_status`, `plugin_free`) e embrulha
   `run`/`status`/`manifest` em `catch_unwind` — um `panic!` atravessando a FFI é
   comportamento indefinido, então **sempre** use a macro.

3. Compile e instale:
   ```sh
   cargo build --release
   cp target/release/lib<nome>.so  CAMINHO/do/bot/plugins/<nome>.so
   ```
   No painel, aba Integrations, clique **Reiniciar bot**. O plugin aparece.

## Contrato JSON

### manifest()  → o card e as ações
```json
{
  "name": "orbital",
  "card": {
    "title": "Orbital Live",
    "color": "#f0795e",
    "icon": "<circle .../><path .../>",   // conteúdo interno de um <svg> 24x24
    "fields": [
      { "key": "host", "label": "Host", "placeholder": "...", "kind": "text", "options": [] }
    ],
    "has_switch": true,    // mostra o liga/desliga (chave "enabled" na config)
    "has_status": true     // mostra o badge e chama status()
  },
  "actions": [
    {
      "name": "fire",
      "label": "orbital_fire",
      "doc": "texto de ajuda mostrado no editor",
      "fields": [
        { "key": "catastrophe", "label": "catastrophe", "kind": "select",
          "placeholder": "", "options": ["nuke","meteor"] }
      ]
    }
  ]
}
```
`kind` de um field: `"text"`, `"number"`, `"password"`, `"select"` (usa `options`), `"bool"`.

### run(ctx)  → executa uma ação
`ctx` recebido:
```json
{ "action": "fire", "user": "fulano",
  "params": { "catastrophe": "nuke" },
  "config": { "enabled": true, "host": "...", "port": 8899, "token": "" } }
```
Retorne `{"ok": true}` em sucesso, ou `{"ok": false, "error": "..."}` — o bot
loga o erro. `params` são os campos da ação (já com `%variáveis%` resolvidas).
`config` é o `plugins/<nome>.config.json`.

A chamada roda numa thread separada (`spawn_blocking`), então pode ser
bloqueante (ex.: `reqwest::blocking`). O plugin é dono da própria concorrência.

### status(cfg)  → badge do card
Recebe a config, retorna `{"enabled": bool, "reachable": bool}`. Mantenha rápido
(use timeout curto); roda a cada atualização do painel.

## ABI e compatibilidade

`ABI_VERSION = 1`. Se o bot subir uma versão nova do contrato, ele **recusa** um
`.so` de versão diferente (loga e ignora), em vez de chamar e arriscar UB.
Recompile o plugin contra o `plugin-sdk` novo quando isso acontecer.

## Segurança — leia

Um `.so`/`.dll` é **código nativo sem sandbox**, rodando com os privilégios do
processo do bot (que roda como root). Carregar um plugin de terceiro é executar
código arbitrário de terceiro na sua máquina. **Só largue em `plugins/` uma
biblioteca que você mesmo compilou ou confia totalmente.** O painel não tem
autenticação e só é seguro atrás do Tailscale — não exponha por Funnel.

Um `panic!` atravessando a FFI é UB e pode derrubar o bot; a macro
`export_plugin!` protege com `catch_unwind`. Não escreva as funções `extern "C"`
na mão sem essa proteção.
