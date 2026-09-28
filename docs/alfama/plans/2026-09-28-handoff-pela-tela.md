# Plano: enviar um handoff pela tela do painel

> Implementa [`../specs/2026-09-28-handoff-pela-tela.md`](../specs/2026-09-28-handoff-pela-tela.md).
> Branch `alfama/feat-handoff-pela-tela`, um PR no fork (não vai para o original — §2 da spec). Vale o
> "Antes de começar" do plano anterior ([`2026-09-24-painel-web-alfama.md`](2026-09-24-painel-web-alfama.md)):
> a função `CARGO`, um build por vez, `df -h /c` antes de compilar, o contorno do Tailwind na montagem do
> Windows.

## Tarefa 1: mover a sanitização do handoff para `ai-memory-core`

**Arquivos:** `crates/ai-memory-core/src/handoff.rs` (perto de `NewHandoff`), `crates/ai-memory-mcp/src/server.rs`.

**Passo 1:** ler `memory_handoff_begin` (server.rs ~4150) por inteiro: `cap_handoff_list`, `cap_text_with_marker`
(comparar com a de `ai-memory-consolidate::projection` — se forem idênticas, reusar essa e não criar uma
terceira cópia), as constantes `HANDOFF_*_MAX_CHARS`, a ordem de aplicação (scrub → cap).

**Passo 2:** criar em `ai_memory_core::handoff` uma função pura `sanitize_handoff_text_fields(sanitizer:
&Sanitizer, summary: &str, open_questions: &[String], next_steps: &[String], files_touched: &[String]) ->
(String, Vec<String>, Vec<String>, Vec<String>)` (ou um struct de entrada/saída, o que ficar mais legível),
com as mesmas constantes de limite movidas para lá. `memory_handoff_begin` passa a chamar essa função em
vez de ter a lógica inline — **comportamento idêntico**, prova por teste (Passo 3).

**Passo 3: testes que fixam a paridade** — um teste em `ai-memory-core` cobrindo os limites e o scrub
isoladamente; um teste em `ai-memory-mcp` que compara o resultado de `memory_handoff_begin` antes/depois do
refactor com a mesma entrada (snapshot dos campos do `NewHandoff` gravado) — deve ser idêntico.

**Gate:** `CARGO test -p ai-memory-core -p ai-memory-mcp` verde. Commit
`refactor(core): share handoff field sanitization between MCP and the web`.

---

## Tarefa 2: `WebState` ganha escrita

**Arquivos:** `crates/ai-memory-web/src/state.rs`, `crates/ai-memory-cli/src/commands/serve.rs` (onde
`WebState::new` é chamado).

**Passo 1:** `WebState` ganha `writer: WriterHandle` e `sanitizer: ai_memory_core::Sanitizer` (os dois já
existem em `serve.rs` para os outros roteadores — só faltam ser passados aqui). `WebState::new` ganha os
dois parâmetros.

**Passo 2:** nenhuma rota existente muda de comportamento — só o construtor. `CARGO test -p ai-memory-web`
continua verde sem nenhuma mudança em outro arquivo. Commit `feat(web): thread the writer and sanitizer
into WebState`.

---

## Tarefa 3: rota, template e CSRF

**Arquivos:** `crates/ai-memory-web/src/routes/handoff_web.rs` (novo), `crates/ai-memory-web/src/routes/mod.rs`
(rota `POST /handoff`), `crates/ai-memory-web/templates/page.html`, `src/templates.rs`
(`PageView` ganha `csrf_token`, `known_projects: Vec<String>`).

**Passo 1: o token CSRF** — módulo novo `crates/ai-memory-web/src/csrf.rs`:
- `CsrfKey` gerado uma vez na subida do processo (`rand::random::<[u8; 32]>()`), guardado em `WebState`
  (adicionar o campo).
- `fn issue(key: &CsrfKey, from_workspace: &str, from_project: &str, from_path: &str, now_unix_minute: i64)
  -> String` — HMAC-SHA256 em hex.
- `fn verify(key: &CsrfKey, token: &str, from_workspace: &str, from_project: &str, from_path: &str,
  now_unix_minute: i64) -> bool` — testa o minuto atual e o anterior (tolerância ~2 min), comparação em
  tempo constante (`subtle::ConstantTimeEq` se já for dependência; senão, comparar bytes com `ct_eq` manual
  simples — **não** adicionar dependência nova só para isto).

**Passo 2: testes do módulo `csrf`** — token do minuto atual verifica; do minuto anterior verifica; de dois
minutos atrás não verifica; com `from_path` diferente do assinado não verifica; com chave diferente não
verifica.

**Passo 3: `page.html`** — `<details>` recolhido "Enviar como handoff", formulário `POST /handoff` com os
campos da spec (§4.2, §4.4), token oculto gerado com `issue(...)` na hora de renderizar, `datalist` dos
projetos conhecidos (reusar a consulta que a página inicial já faz, sem SQL novo).

**Passo 4: a rota** — `handle_create_handoff_web`:
1. Confere `csrf_token` com `verify(...)` usando `from_workspace/from_project/from_path` **do corpo do
   POST**; falha → 403, nada é escrito.
2. Confere `Origin` (quando presente) contra o `Host` esperado; falha → 403.
3. `ai_memory_store::ScopeResolver::new(&state.reader, ws_atual, proj_atual).with_writer(&state.writer)`
   para resolver/criar `(to_workspace, to_project)` a partir do formulário — **nunca** a partir de
   `from_workspace/from_project`, que vêm só do campo oculto preenchido pelo servidor.
4. `sanitize_handoff_text_fields` (Tarefa 1) nos campos de texto.
5. `ai_memory_core::owner_stamp` para `owner_user`, igual ao MCP (`shared` do formulário decide `None` ou
   o carimbo).
6. `state.wiki.authorize_operation(ws_destino, proj_destino, AdmissionOp::HandoffBegin, ...)` — mesma
   política de admissão do MCP; recusa → mensagem de erro, nada gravado.
7. `state.writer.insert_handoff(NewHandoff { from_session_id: None, from_agent: AgentKind::Other, ... })`
   com `next_steps` prefixado pelo link da página de origem (`page_href(from_workspace, from_project,
   from_path)`), como pedido na spec.
8. `303 See Other` de volta para a página de origem, com `?handoff=enviado` ou `?handoff=erro`.

**Passo 5: testes de rota, todos adversariais** (spec §5):
- sem `csrf_token` → 403, nada no `memory_handoff_list` de destino;
- `csrf_token` de outra página (`from_path` diferente) → 403;
- `csrf_token` de 5 minutos atrás → 403;
- `Origin` de outro host → 403;
- controle: token válido, origem correta → grava e redireciona; conferir com uma leitura direta do reader
  que o handoff existe, com o resumo sanitizado e o link da página de origem no primeiro item de
  `next_steps`;
- `from_workspace`/`from_project` forjados no corpo (diferentes do que a página realmente é) são
  ignorados — o handoff nasce com o escopo de origem real;
- destino com nome novo cria o projeto; destino existente não duplica;
- um `<script>` no resumo sai escapado no handoff gravado (mesma prova que o MCP já tem);
- um webhook de admissão com política de recusa também recusa aqui.
- Confirmar que os testes de CSRF mordem: comentar a chamada a `verify(...)` na rota, rodar o teste "sem
  token", ver falhar (a requisição passaria e gravaria), restaurar.

**Gate:** `CARGO fmt --all -- --check`, `git diff --check`, `CARGO clippy --workspace --all-targets -- -D
warnings`, `CARGO test -p ai-memory-web` verde, depois `CARGO test --workspace --exclude ai-memory-cli`
completo. Tailwind: se `page.html` usar classe nova, regenerar e conferir byte a byte (procedimento do
plano anterior). Commit `feat(web): send a page as a handoff from the panel screen, CSRF-protected`.

---

## Tarefa 4: documentação e checklist de segurança

**Arquivos:** `docs/alfama/specs/2026-09-24-painel-web-alfama-design.md` (§2.1, acrescentar a exceção
nomeada desta rota ao item "nenhuma escrita"), `docs/security-boundaries.md` (linha nova para o painel:
CSRF + sanitização + admissão, citando os testes da Tarefa 3), `CHANGELOG.md` `### Added`.

---

## Tarefa 5: gate, imagem e PR

1. Gate completo, um comando por vez.
2. Imagem `ai-memory-alfama:2.4.1-alfama.N`; backup e troca no servidor.
3. Verificação da spec (as 3 etapas do "Verificação").
4. PR no fork para `alfama/main`, com o corpo pelo template e o checklist §2.1 já com a exceção desta
   rota, autoria conferida.
