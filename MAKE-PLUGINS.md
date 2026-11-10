# MAKE-PLUGINS.md — how to build plugins for nuggtbot

Complete guide to **nuggtbot** plugins, for people and for AI agents.
After reading this you should be able to build a plugin from scratch, with actions,
its own config and even its own overlays / browser docks, without touching the
bot's code.

> Quick code reference: the SDK lives in `plugin-sdk/`, the full example in
> `plugin-sdk/orbital-example/`. This document is the primary source; the
> `plugin-sdk/README.md` is a shorter summary.

---

## 1. What a plugin is (and the separation guarantee)

A plugin is a **separate Rust crate**, compiled as a native C-ABI library
(`cdylib` → `.so` on Linux, `.dll` on Windows). You compile the plugin **outside** the
bot and drop the finished file into the `plugins/` folder. The bot loads plugins at
boot.

**Plugins are fully separate from the backend. Concrete guarantees:**

- The bot's `Cargo.toml` **does not depend on any plugin nor on the `plugin-sdk`**. The
  only related dependency is `libloading` (loading `.so`/`.dll` at runtime).
- **There is no workspace** tying the bot and plugins together: compiling the bot never compiles a
  plugin, and compiling a plugin never compiles the bot.
- Adding or removing a plugin **never** touches, recompiles or restarts the bot's
  build. You only need to restart the process (the **Restart bot** button in the panel) for
  it to re-read the `plugins/` folder.
- The boundary between bot and plugin exchanges **only JSON inside a C-string**.
  Bot and plugin **share no Rust type**. They can be compiled with
  different versions of the compiler and of dependencies.
- `plugins/*.so`, `plugins/*.dll` and `plugins/*.config.json` are **gitignored**: the
  plugin binary and its config (hosts, tokens) never enter the repository. The
  source code of the SDK and of the example (`plugin-sdk/`) is versioned.

In other words: a plugin is an isolated, pluggable piece. If the `.so` disappears, the bot runs
the same, just without that plugin.

---

## 2. Architecture (the lifecycle)

1. **Bot boot** → `plugin::load_all()` scans `plugins/`, loads each
   `*.so`/`*.dll` whose ABI matches `ABI_VERSION` (currently `1`). A broken binary or
   one with a different ABI is **logged and ignored**, never crashes the bot.
2. Each loaded plugin exposes its **manifest** (a JSON with `name`, `card`,
   `actions` and, optionally, `pages`).
3. The panel, in the **Integrations → Plugins** tab, shows one **card** per plugin
   (with an on/off switch and a status badge) and lists the plugin's **actions** in the
   Actions editor.
4. When a plugin action is triggered (by command, reward, timer, etc.), the
   bot calls `plugin_run` with `{action,user,params,config}`.
5. The card's badge calls `plugin_status` with the config, periodically.
6. If the plugin declares pages, they are served at `/plugin/<name>/<path>`
   (see section 7).

Everything crossing the boundary is JSON. The plugin owns its own I/O and concurrency.

---

## 3. The exported functions (the C-ABI contract)

You **do not write** the `extern "C"` functions by hand. Implement normal Rust
functions and call the `plugin_sdk::export_plugin!` macro. It generates the C functions and
protects each one with `catch_unwind` — **a `panic!` crossing the FFI is
undefined behavior (UB)** and can crash the bot, so always use the macro.

Exported functions:

| C function           | Generated from | Receives (JSON)            | Returns (JSON)                         |
|----------------------|--------------|----------------------------|----------------------------------------|
| `plugin_abi_version` | (automatic)  | —                          | `u32` (the ABI version)                |
| `plugin_manifest`    | `manifest`   | —                          | `{name, card, actions, ...}`           |
| `plugin_run`         | `run`        | `{action,user,params,config}` | `{ok:bool, error?}`                 |
| `plugin_status`      | `status`     | `config`                   | `{enabled:bool, reachable:bool}`       |
| `plugin_free`        | (automatic)  | pointer                    | —                                      |
| `plugin_page`        | `page` (optional) | `{path, config}`      | `{ok:bool, body, content_type?}`       |

---

## 4. Assembling a plugin (step by step)

### 4.1 Crate

`Cargo.toml`:
```toml
[package]
name = "meuplugin"
version = "0.1.0"
edition = "2021"

[lib]
# Gera a biblioteca C-ABI: libmeuplugin.so (Linux) / meuplugin.dll (Windows).
crate-type = ["cdylib"]

[dependencies]
plugin-sdk = { path = "CAMINHO/para/plugin-sdk" }
serde_json = "1"
# + o que seu plugin precisar, ex.:
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "blocking", "json"] }

[profile.release]
strip = true
opt-level = "z"
lto = true
```

### 4.2 Minimal code

```rust
use serde_json::{json, Value};

fn manifest() -> Value {
    json!({
        "name": "meuplugin",
        "card": { "title": "Meu Plugin", "color": "#59c3c3", "fields": [], "has_switch": true, "has_status": true },
        "actions": []
    })
}
fn run(_ctx: Value) -> Value { json!({ "ok": true }) }
fn status(cfg: Value) -> Value {
    json!({ "enabled": cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false), "reachable": true })
}

plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status);
```

### 4.3 Build and install

```sh
cargo build --release
cp target/release/libmeuplugin.so  CAMINHO/do/bot/plugins/meuplugin.so
```
In the panel → **Integrations → Plugins → Restart bot**. The card appears.

---

## 5. The manifest in detail

```jsonc
{
  "name": "orbital",                 // único, [a-zA-Z0-9_-]; é a chave do plugin e do arquivo de config
  "card": {
    "title": "Orbital Live",
    "color": "#f0795e",
    "icon": "<circle .../><path .../>",   // conteúdo INTERNO de um <svg> 24x24 (stroke currentColor)
    "fields": [                            // campos de config editáveis no card
      { "key": "host", "label": "Host", "placeholder": "100.x.y.z", "kind": "text", "options": [] }
    ],
    "has_switch": true,                    // mostra o liga/desliga (chave "enabled" na config)
    "has_status": true,                    // mostra o badge e chama status()
    "pages": [                             // OPCIONAL: overlays/docks próprios (ver seção 7)
      { "label": "Overlay", "path": "overlay" }
    ]
  },
  "actions": [
    {
      "name": "fire",                      // nome interno da ação
      "label": "orbital_fire",             // como aparece no editor de Actions
      "doc": "texto de ajuda",
      "fields": [
        { "key": "catastrophe", "label": "catastrophe", "kind": "select", "placeholder": "", "options": ["nuke","meteor"] }
      ]
    }
  ]
}
```

A field's `kind`: `"text"`, `"number"`, `"password"`, `"select"` (uses `options`),
`"bool"`.

---

## 6. run / status / config

### run(ctx) — executes an action
The received `ctx`:
```json
{ "action": "fire", "user": "fulano",
  "params": { "catastrophe": "nuke" },
  "config": { "enabled": true, "host": "...", "port": 8899, "token": "" } }
```
- `params` are the action's fields, **already with `%variables%` resolved** by the bot.
- `config` is the content of `plugins/<name>.config.json`.
- Return `{"ok": true}` on success, or `{"ok": false, "error": "..."}` — the bot
  logs the error.
- Convention: if `config.enabled` is `false`, return `{"ok": true}` and do
  nothing (the plugin is off).
- `run` runs on a separate thread (`spawn_blocking`), so it **may block**
  (e.g. `reqwest::blocking`). The plugin owns its own concurrency.

### status(cfg) — card badge
Receives the config, returns `{"enabled": bool, "reachable": bool}`. **Keep it
fast** (short timeout, e.g. 2s); it is called on every panel refresh.

### Config
`plugins/<name>.config.json` is created/edited by the panel from the card's `fields`.
The bot passes this config on **every** call (`run`, `status`, `page`). The
plugin never writes to this file — it only reads what arrives in the `ctx`.

---

## 7. Your own overlays and browser docks (the `page` capability)

A plugin can serve **its own HTML pages** — overlays that appear on the
stream, or browser docks you place in OBS. This allows complete visual integrations
without touching the bot.

### How it works
1. Declare the pages in the manifest, under `card.pages`:
   ```json
   "pages": [ { "label": "Overlay", "path": "overlay" }, { "label": "Dock", "path": "dock" } ]
   ```
   The panel shows, on the plugin's card, each page's URL ready to copy.
2. Implement the `page` function and pass it to the macro:
   ```rust
   fn page(ctx: Value) -> Value {
       let path = ctx.get("path").and_then(|p| p.as_str()).unwrap_or("");
       let body = match path {
           "overlay" => "<!doctype html><html>…seu overlay…</html>".to_string(),
           "dock"    => "<!doctype html><html>…seu dock…</html>".to_string(),
           _ => return json!({ "ok": false }),   // 404
       };
       json!({ "ok": true, "body": body })        // content_type default: text/html
   }
   plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status, page = page);
   ```
3. The page is served at `GET /plugin/<name>/<path>`, e.g.:
   `http://127.0.0.1:7777/plugin/orbital/overlay`. Paste that URL as a **Browser
   Source** in OBS.

### `page` return value
```json
{ "ok": true, "body": "<...conteúdo...>", "content_type": "text/html; charset=utf-8" }
```
- `body`: the served content (usually a full HTML document).
- `content_type`: optional, default `text/html; charset=utf-8`. Use another to
  serve CSS/JS/JSON from the same function (e.g. `path == "style.css"` →
  `"content_type": "text/css"`).
- `{"ok": false}` (or a missing symbol) → the bot responds **404**.
- `ctx.config` is available, so you can inject a token/host from the config
  into the HTML.

### Live data in your pages
The plugin's page is served **from the same origin** as the panel. So its
JavaScript can consume the same feeds the native overlays/docks use, without
any new API:
- `new EventSource('/events')` → chat messages (`message`, `delete`,
  `clear` events). It's the same feed as the native chat overlay.
- `new WebSocket('ws://'+location.host+'/ws')` → events (subs, bits, raids,
  rewards, etc.).
- `fetch('/api/...')` → any panel endpoint.

Example of a plugin overlay that reads chat:
```html
<!doctype html><meta charset="utf-8"><ul id="log"></ul>
<script>
  const log = document.getElementById('log');
  new EventSource('/events').addEventListener('message', e => {
    const m = JSON.parse(e.data);
    const li = document.createElement('li');
    li.textContent = m.from + ': ' + (m.parts||[]).map(p => p.type==='text'?p.text:'[emote]').join('');
    log.appendChild(li);
  });
</script>
```

---

## 8. How the plugin action is used in the bot

In the panel's **Actions** editor, the plugin action appears as a sub-action
with the synthetic `kind` `plugin:<name>:<action>` and the `fields` you declared. In
storage, it becomes `{kind:"plugin", plugin, action, params:{...}}`. You wire
that Action to a command, reward, timer or event like any other. When it
fires, the bot resolves the params' `%variables%` and calls your `run`.

---

## 9. ABI and compatibility

- `ABI_VERSION = 1`. If the bot ships a new version of the contract, it **refuses** a
  `.so` of a different version (logs and ignores it) instead of calling it and risking UB.
  Recompile the plugin against the new `plugin-sdk` when that happens.
- The `plugin_page` function is **optional and additive**: old plugins that don't
  export it stay valid under ABI 1. Omitting `page` in the macro keeps the plugin with
  exactly the five base functions.

---

## 10. Security — read this

A `.so`/`.dll` is **native code with no sandbox**, running with the privileges of the
bot process. Loading a third-party plugin is **running arbitrary third-party code on
your machine**. Only drop into `plugins/` a library you compiled yourself or trust
fully.

- Plugin pages (`/plugin/<name>/...`) run **from the same origin** as the panel
  and can read `/events`, `/ws` and `/api/*`. Since the plugin is already native code
  that is fully trusted in the process, serving HTML from it **adds no privilege**
  beyond what it already has.
- The panel has **no authentication** and is only safe **behind Tailscale** — do not
  expose it via Funnel, do not bind to `0.0.0.0` on an untrusted network. The same applies
  equally to plugin pages.
- Never let a `panic!` cross the FFI: always use `export_plugin!` (it
  protects with `catch_unwind`). Do not write the `extern "C"` functions by hand.
- Keep `status()` and `page()` with short timeouts; they run at the request of the
  panel/OBS and must not hang.

---

## 11. Checklist for a new plugin

- [ ] `cdylib` crate with `plugin-sdk` and `serde_json`.
- [ ] `manifest`, `run`, `status` implemented; `page` if you have an overlay/dock.
- [ ] `plugin_sdk::export_plugin!(...)` calling the functions.
- [ ] Unique `name`, only `[a-zA-Z0-9_-]`.
- [ ] `run` returns `{ok:false,error}` on failure and respects `config.enabled`.
- [ ] `status`/`page` fast (short timeout).
- [ ] `cargo build --release`, copy the `.so`/`.dll` to `plugins/`, **Restart
      bot** in the panel.
- [ ] Check the card in the Integrations tab and, if present, open the page's URL.

---

## Português (Brasil)

Guia completo de plugins do **nuggtbot**, para pessoas e para agentes de IA.
Depois de ler isto você deve conseguir criar um plugin do zero, com ações,
config própria e até overlays / browser docks próprios, sem tocar no código do
bot.

> Referência rápida de código: a SDK fica em `plugin-sdk/`, o exemplo completo em
> `plugin-sdk/orbital-example/`. Este documento é a fonte principal; o
> `plugin-sdk/README.md` é um resumo mais curto.

### 1. O que é um plugin (e a garantia de separação)

Um plugin é um **crate Rust separado**, compilado como biblioteca nativa C-ABI
(`cdylib` → `.so` no Linux, `.dll` no Windows). Você compila o plugin **fora** do
bot e larga o arquivo pronto na pasta `plugins/`. O bot carrega os plugins no
boot.

**Os plugins são totalmente separados do backend. Garantias concretas:**

- O `Cargo.toml` do bot **não depende de nenhum plugin nem do `plugin-sdk`**. A
  única dependência relacionada é `libloading` (carregar `.so`/`.dll` em runtime).
- **Não existe workspace** unindo bot e plugins: compilar o bot nunca compila um
  plugin, e compilar um plugin nunca compila o bot.
- Adicionar ou remover um plugin **nunca** mexe, recompila ou reinicia o build do
  bot. Só é preciso reiniciar o processo (botão **Reiniciar bot** no painel) para
  ele reler a pasta `plugins/`.
- A fronteira entre bot e plugin troca **apenas JSON dentro de uma C-string**.
  Bot e plugin **não compartilham nenhum tipo Rust**. Podem ser compilados com
  versões diferentes de compilador e de dependências.
- `plugins/*.so`, `plugins/*.dll` e `plugins/*.config.json` são **gitignored**: o
  binário do plugin e sua config (hosts, tokens) nunca entram no repositório. O
  código-fonte da SDK e do exemplo (`plugin-sdk/`) é versionado.

Ou seja: um plugin é uma peça isolada e plugável. Se o `.so` sumir, o bot roda
igual, só sem aquele plugin.

### 2. Arquitetura (o ciclo de vida)

1. **Boot do bot** → `plugin::load_all()` varre `plugins/`, carrega cada
   `*.so`/`*.dll` cujo ABI bate com `ABI_VERSION` (hoje `1`). Binário quebrado ou
   de ABI diferente é **logado e ignorado**, nunca derruba o bot.
2. Cada plugin carregado expõe seu **manifest** (um JSON com `name`, `card`,
   `actions` e, opcionalmente, `pages`).
3. O painel, na aba **Integrations → Plugins**, mostra um **card** por plugin
   (com switch liga/desliga e badge de status) e lista as **ações** do plugin no
   editor de Actions.
4. Quando uma ação do plugin é disparada (por comando, reward, timer, etc.), o
   bot chama `plugin_run` com `{action,user,params,config}`.
5. O badge do card chama `plugin_status` com a config, de tempos em tempos.
6. Se o plugin declara páginas, elas são servidas em `/plugin/<nome>/<path>`
   (ver seção 7).

Tudo que cruza a fronteira é JSON. O plugin é dono da própria I/O e concorrência.

### 3. As funções exportadas (o contrato C-ABI)

Você **não escreve** as funções `extern "C"` na mão. Implemente funções Rust
normais e chame a macro `plugin_sdk::export_plugin!`. Ela gera as funções C e
protege cada uma com `catch_unwind` — **um `panic!` atravessando a FFI é
comportamento indefinido (UB)** e pode derrubar o bot, então sempre use a macro.

Funções exportadas:

| Função C             | Gerada de    | Recebe (JSON)              | Retorna (JSON)                         |
|----------------------|--------------|----------------------------|----------------------------------------|
| `plugin_abi_version` | (automático) | —                          | `u32` (a versão do ABI)                |
| `plugin_manifest`    | `manifest`   | —                          | `{name, card, actions, ...}`           |
| `plugin_run`         | `run`        | `{action,user,params,config}` | `{ok:bool, error?}`                 |
| `plugin_status`      | `status`     | `config`                   | `{enabled:bool, reachable:bool}`       |
| `plugin_free`        | (automático) | ponteiro                   | —                                      |
| `plugin_page`        | `page` (opcional) | `{path, config}`      | `{ok:bool, body, content_type?}`       |

### 4. Montando um plugin (passo a passo)

#### 4.1 Crate

`Cargo.toml`:
```toml
[package]
name = "meuplugin"
version = "0.1.0"
edition = "2021"

[lib]
# Gera a biblioteca C-ABI: libmeuplugin.so (Linux) / meuplugin.dll (Windows).
crate-type = ["cdylib"]

[dependencies]
plugin-sdk = { path = "CAMINHO/para/plugin-sdk" }
serde_json = "1"
# + o que seu plugin precisar, ex.:
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "blocking", "json"] }

[profile.release]
strip = true
opt-level = "z"
lto = true
```

#### 4.2 Código mínimo

```rust
use serde_json::{json, Value};

fn manifest() -> Value {
    json!({
        "name": "meuplugin",
        "card": { "title": "Meu Plugin", "color": "#59c3c3", "fields": [], "has_switch": true, "has_status": true },
        "actions": []
    })
}
fn run(_ctx: Value) -> Value { json!({ "ok": true }) }
fn status(cfg: Value) -> Value {
    json!({ "enabled": cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false), "reachable": true })
}

plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status);
```

#### 4.3 Compilar e instalar

```sh
cargo build --release
cp target/release/libmeuplugin.so  CAMINHO/do/bot/plugins/meuplugin.so
```
No painel → **Integrations → Plugins → Reiniciar bot**. O card aparece.

### 5. O manifest em detalhe

```jsonc
{
  "name": "orbital",                 // único, [a-zA-Z0-9_-]; é a chave do plugin e do arquivo de config
  "card": {
    "title": "Orbital Live",
    "color": "#f0795e",
    "icon": "<circle .../><path .../>",   // conteúdo INTERNO de um <svg> 24x24 (stroke currentColor)
    "fields": [                            // campos de config editáveis no card
      { "key": "host", "label": "Host", "placeholder": "100.x.y.z", "kind": "text", "options": [] }
    ],
    "has_switch": true,                    // mostra o liga/desliga (chave "enabled" na config)
    "has_status": true,                    // mostra o badge e chama status()
    "pages": [                             // OPCIONAL: overlays/docks próprios (ver seção 7)
      { "label": "Overlay", "path": "overlay" }
    ]
  },
  "actions": [
    {
      "name": "fire",                      // nome interno da ação
      "label": "orbital_fire",             // como aparece no editor de Actions
      "doc": "texto de ajuda",
      "fields": [
        { "key": "catastrophe", "label": "catastrophe", "kind": "select", "placeholder": "", "options": ["nuke","meteor"] }
      ]
    }
  ]
}
```

`kind` de um field: `"text"`, `"number"`, `"password"`, `"select"` (usa `options`),
`"bool"`.

### 6. run / status / config

#### run(ctx) — executa uma ação
`ctx` recebido:
```json
{ "action": "fire", "user": "fulano",
  "params": { "catastrophe": "nuke" },
  "config": { "enabled": true, "host": "...", "port": 8899, "token": "" } }
```
- `params` são os campos da ação, **já com `%variáveis%` resolvidas** pelo bot.
- `config` é o conteúdo de `plugins/<nome>.config.json`.
- Retorne `{"ok": true}` em sucesso, ou `{"ok": false, "error": "..."}` — o bot
  loga o erro.
- Convenção: se `config.enabled` for `false`, retorne `{"ok": true}` e não faça
  nada (o plugin está desligado).
- `run` roda numa thread separada (`spawn_blocking`), então **pode bloquear**
  (ex.: `reqwest::blocking`). O plugin é dono da própria concorrência.

#### status(cfg) — badge do card
Recebe a config, retorna `{"enabled": bool, "reachable": bool}`. **Mantenha
rápido** (timeout curto, ex. 2s); é chamado a cada atualização do painel.

#### Config
`plugins/<nome>.config.json` é criado/editado pelo painel a partir dos `fields`
do card. O bot passa essa config em **toda** chamada (`run`, `status`, `page`). O
plugin nunca escreve nesse arquivo — só lê o que chega no `ctx`.

### 7. Overlays e browser docks próprios (capacidade `page`)

Um plugin pode servir **suas próprias páginas HTML** — overlays que aparecem na
live, ou browser docks que você coloca no OBS. Isso permite integrações visuais
completas sem tocar no bot.

#### Como funciona
1. Declare as páginas no manifest, em `card.pages`:
   ```json
   "pages": [ { "label": "Overlay", "path": "overlay" }, { "label": "Dock", "path": "dock" } ]
   ```
   O painel mostra, no card do plugin, a URL de cada página pronta para copiar.
2. Implemente a função `page` e passe-a para a macro:
   ```rust
   fn page(ctx: Value) -> Value {
       let path = ctx.get("path").and_then(|p| p.as_str()).unwrap_or("");
       let body = match path {
           "overlay" => "<!doctype html><html>…seu overlay…</html>".to_string(),
           "dock"    => "<!doctype html><html>…seu dock…</html>".to_string(),
           _ => return json!({ "ok": false }),   // 404
       };
       json!({ "ok": true, "body": body })        // content_type default: text/html
   }
   plugin_sdk::export_plugin!(manifest = manifest(), run = run, status = status, page = page);
   ```
3. A página é servida em `GET /plugin/<nome>/<path>`, ex.:
   `http://127.0.0.1:7777/plugin/orbital/overlay`. Cole essa URL como **Browser
   Source** no OBS.

#### Retorno de `page`
```json
{ "ok": true, "body": "<...conteúdo...>", "content_type": "text/html; charset=utf-8" }
```
- `body`: o conteúdo servido (geralmente um documento HTML completo).
- `content_type`: opcional, default `text/html; charset=utf-8`. Use outro para
  servir CSS/JS/JSON na mesma função (ex. `path == "style.css"` →
  `"content_type": "text/css"`).
- `{"ok": false}` (ou símbolo ausente) → o bot responde **404**.
- `ctx.config` está disponível, então você pode injetar um token/host da config
  dentro do HTML.

#### Dados ao vivo nas suas páginas
A página do plugin é servida **na mesma origem** do painel. Então o JavaScript
dela pode consumir os mesmos feeds que os overlays/docks nativos usam, sem
nenhuma API nova:
- `new EventSource('/events')` → mensagens de chat (eventos `message`, `delete`,
  `clear`). É o mesmo feed do overlay de chat nativo.
- `new WebSocket('ws://'+location.host+'/ws')` → eventos (subs, bits, raids,
  rewards, etc.).
- `fetch('/api/...')` → qualquer endpoint do painel.

Exemplo de overlay de plugin que lê o chat:
```html
<!doctype html><meta charset="utf-8"><ul id="log"></ul>
<script>
  const log = document.getElementById('log');
  new EventSource('/events').addEventListener('message', e => {
    const m = JSON.parse(e.data);
    const li = document.createElement('li');
    li.textContent = m.from + ': ' + (m.parts||[]).map(p => p.type==='text'?p.text:'[emote]').join('');
    log.appendChild(li);
  });
</script>
```

### 8. Como a ação do plugin é usada no bot

No editor de **Actions** do painel, a ação do plugin aparece como uma sub-action
com `kind` sintético `plugin:<nome>:<ação>` e os `fields` que você declarou. No
armazenamento, vira `{kind:"plugin", plugin, action, params:{...}}`. Você liga
essa Action a um comando, reward, timer ou evento como qualquer outra. Quando
dispara, o bot resolve as `%variáveis%` dos params e chama o seu `run`.

### 9. ABI e compatibilidade

- `ABI_VERSION = 1`. Se o bot subir uma versão nova do contrato, ele **recusa** um
  `.so` de versão diferente (loga e ignora) em vez de chamar e arriscar UB.
  Recompile o plugin contra o `plugin-sdk` novo quando isso acontecer.
- A função `plugin_page` é **opcional e aditiva**: plugins antigos que não a
  exportam continuam válidos no ABI 1. Omitir `page` na macro mantém o plugin com
  exatamente as cinco funções base.

### 10. Segurança — leia

Um `.so`/`.dll` é **código nativo sem sandbox**, rodando com os privilégios do
processo do bot. Carregar um plugin de terceiro é **executar código arbitrário de
terceiro na sua máquina**. Só largue em `plugins/` uma biblioteca que você mesmo
compilou ou confia totalmente.

- As páginas de plugin (`/plugin/<nome>/...`) rodam **na mesma origem** do painel
  e podem ler `/events`, `/ws` e `/api/*`. Como o plugin já é código nativo
  totalmente confiável no processo, servir HTML dele **não adiciona privilégio**
  nenhum além do que ele já tem.
- O painel **não tem autenticação** e só é seguro **atrás do Tailscale** — não
  exponha por Funnel, não faça bind em `0.0.0.0` em rede não confiável. Isso vale
  igualmente para as páginas de plugin.
- Nunca deixe um `panic!` atravessar a FFI: use sempre `export_plugin!` (ela
  protege com `catch_unwind`). Não escreva as funções `extern "C"` na mão.
- Mantenha `status()` e `page()` com timeouts curtos; eles rodam a pedido do
  painel/OBS e não devem travar.

### 11. Checklist para um plugin novo

- [ ] Crate `cdylib` com `plugin-sdk` e `serde_json`.
- [ ] `manifest`, `run`, `status` implementados; `page` se tiver overlay/dock.
- [ ] `plugin_sdk::export_plugin!(...)` chamando as funções.
- [ ] `name` único, só `[a-zA-Z0-9_-]`.
- [ ] `run` devolve `{ok:false,error}` em falha e respeita `config.enabled`.
- [ ] `status`/`page` rápidos (timeout curto).
- [ ] `cargo build --release`, copiar o `.so`/`.dll` para `plugins/`, **Reiniciar
      bot** no painel.
- [ ] Conferir o card na aba Integrations e, se houver, abrir a URL da página.
