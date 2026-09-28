# Enviar um handoff pela tela do painel

> Spec do fork `maxeinstein-dev/ai-memory`. Continuação de
> [`2026-09-24-painel-web-alfama-design.md`](2026-09-24-painel-web-alfama-design.md): mesmo fluxo de
> trabalho (§2.2). Pedido do usuário em 2026-09-28, depois de eu ter criado manualmente, pela ferramenta
> de memória, um handoff levando o conteúdo de uma página do `aw-senai-sgw` para o projeto `iel`.

## 1. O problema

Hoje, para levar o contexto de uma página para outra sessão ou outro projeto, é preciso pedir ao agente
para chamar `memory_handoff_begin` pela ferramenta de memória. Funciona, mas exige que alguém com acesso
ao MCP monte a chamada. O usuário quer fazer isso **direto na tela**, abrindo a página e clicando num
botão.

## 2. Por que isso muda a natureza do painel

O `/web`, no original e no fork, é **só leitura por desenho** — é item do checklist de segurança de todo
PR do painel (§2.1 da spec do painel: "nenhuma escrita fora do `WriterHandle`"). Esta é a **primeira
funcionalidade de escrita**. Por isso:

- **Fica só no fork.** O original documenta o `/web` como intencionalmente só leitura ("no new
  authentication surface"). Não vai como contribuição para o Akita.
- **O checklist de segurança do painel ganha uma exceção explícita e nomeada**, só para esta rota: ela
  escreve, tudo o mais continua valendo (nada fora do `WriterHandle`, sanitização obrigatória).
- **Introduz um risco novo que o painel nunca teve: CSRF.** Um formulário que faz `POST` sem token pode
  ser disparado por uma página maliciosa aberta numa aba, contra o servidor local (`127.0.0.1:49374`),
  sem o navegador barrar o envio (só barra a *leitura* da resposta, por causa do CORS). Como o servidor
  não usa cookie de sessão, não há defesa de origem nativa. Esta spec exige uma defesa própria (§4.3).

## 3. Decisões do usuário (2026-09-28)

- **Onde:** um botão "Enviar como handoff" na tela de cada página (`page.html`).
- **Destino:** um campo que mistura lista suspensa e texto livre (`<input list>` + `<datalist>` dos
  projetos existentes); digitar um nome novo cria o projeto, como `memory_handoff_begin` já faz hoje.
- **Conteúdo:** formulário com campos — resumo, próximos passos (um por linha), perguntas em aberto (um
  por linha) — com a página de origem já sugerida como primeiro link. Não é um botão de um clique só.

## 4. Desenho

### 4.1. Reaproveitamento (não duplicar lógica de segurança)

Investigado em `crates/ai-memory-mcp/src/server.rs::memory_handoff_begin` (linha ~4150) — o que ele faz e
onde cada peça mora:

| Peça | Onde já mora | Reaproveitável pela web sem mudança? |
|---|---|---|
| Resolver/criar `(workspace, project)` | `ai_memory_store::ScopeResolver::resolve_write_args` (`scope.rs`) | **Sim.** Já é `ai-memory-store`, dependência da web. `ScopeResolver::new(&reader, ws_ativo, proj_ativo).with_writer(&writer)` |
| Carimbo de dono (`owner_user`) | `ai_memory_core::owner_stamp` (`actor.rs`) | **Sim.** `ai-memory-core` já é dependência da web (`ActorContext` já é usado em `api.rs`) |
| Disparo de admissão (`AdmissionOp::HandoffBegin`) | `ai_memory_wiki::Wiki::authorize_operation` | **Sim.** `WebState` já guarda um `Wiki` |
| Sanitização + limites (`cap_handoff_list`, `cap_text_with_marker`, `HANDOFF_*_MAX_CHARS`) | privados em `ai-memory-mcp/src/server.rs` | **Não hoje** — precisa mudar de lugar |
| Gravação (`insert_handoff`) | `WriterHandle::insert_handoff` (`writer.rs`) | **Sim**, mas a web não tem `WriterHandle` hoje |

**Mudança de estrutura necessária, mínima:**

1. Mover `cap_handoff_list`, `cap_text_with_marker` (a versão do `ai-memory-mcp`, que já existe em
   `ai-memory-consolidate::projection` — conferir se são a mesma função antes de duplicar) e as constantes
   `HANDOFF_SUMMARY_MAX_CHARS`/`HANDOFF_ITEM_MAX_CHARS`/`HANDOFF_TEXT_LIST_MAX_CHARS`/
   `HANDOFF_FILE_MAX_CHARS`/`HANDOFF_FILE_LIST_MAX_CHARS` para `ai_memory_core::handoff` (perto de
   `NewHandoff`), com uma função só, `sanitize_new_handoff_fields(sanitizer, args) -> NewHandoff` (ou
   próximo disso), que o MCP e a web chamam **os dois**. Mesmo princípio já usado para o `brief.rs`
   (Tarefa 1 da spec do painel): paridade estrutural, não por teste que repete a lógica.
   `crates/ai-memory-mcp/src/server.rs::memory_handoff_begin` passa a chamar essa função também, em vez de
   ter a sua própria cópia.
2. `WebState` ganha `writer: WriterHandle` e `sanitizer: Sanitizer` (os dois já existem no processo
   `serve`, só faltam ser passados para o `WebState` na montagem em `serve.rs`).

### 4.2. Rota

`POST /handoff` (top-level, como `/entre-projetos`), corpo `application/x-www-form-urlencoded`:

| Campo | Origem | Observação |
|---|---|---|
| `from_workspace`, `from_project` | campo oculto, preenchido pelo servidor ao renderizar `page.html` | **nunca vem do que o usuário digitou** — é o escopo da própria página aberta |
| `from_path` | campo oculto | caminho da página de origem, vira o primeiro link do `next_steps` |
| `to_workspace`, `to_project` | formulário (`datalist` + texto livre) | resolvido/criado por `ScopeResolver::resolve_write_args` |
| `summary`, `next_steps`, `open_questions` | formulário (textarea, um item por linha) | sanitizados e limitados como no MCP |
| `shared` | checkbox, marcado por padrão | igual ao parâmetro do `memory_handoff_begin` |
| `csrf_token` | campo oculto, gerado ao renderizar `page.html` | ver §4.3 |

Resposta: redireciona (`303 See Other`) de volta para a página de origem, com um aviso de sucesso
(`?handoff=enviado`) ou de erro (`?handoff=erro&motivo=...`) — nunca um corpo de erro cru.

### 4.3. Defesa contra CSRF

Token com HMAC, sem estado no servidor (sem tabela de sessão nova):

- **Geração:** ao renderizar `page.html`, o servidor calcula `token = HMAC-SHA256(chave_do_processo,
  "{from_workspace}\0{from_project}\0{from_path}\0{minuto_unix}")`, embutido num campo oculto do
  formulário. `chave_do_processo` é gerada uma vez, na subida do servidor (`rand`, já é dependência
  transitiva), guardada só em memória — não precisa sobreviver a um restart.
- **Verificação no `POST`:** recalcula o HMAC para os minutos `{atual, atual-1}` (tolerância de até ~2
  min) com os mesmos `from_workspace/from_project/from_path` **do corpo do POST**, e compara em tempo
  constante. Token que não bate, ou mais velho que isso → 403, nada é escrito.
- **Por que isso funciona:** uma página de outra origem consegue *mandar* o `POST` (o navegador não
  impede o envio), mas não consegue *ler* o token, porque ler a resposta do `GET` de `page.html` é
  bloqueado pelo CORS — a origem maliciosa nunca sabe qual token embutir.
- **Camada extra:** conferir o cabeçalho `Origin` (quando presente) contra o `Host` esperado do próprio
  bind, do mesmo jeito que o guard de Host já existente nas outras rotas. Ausência de `Origin` (alguns
  clientes não mandam em `POST` same-origin) não derruba a requisição sozinha — só o token decide.

### 4.4. Template (`page.html`)

- Um `<details>` recolhido por padrão ("Enviar como handoff"), para não poluir a leitura normal da página.
- `datalist` populado pela mesma listagem de projetos que a página inicial já usa (`state.reader`, sem
  consulta nova).
- Nenhum `|safe`: os textos do formulário voltam escapados pelo askama, como em toda tela do painel.
- Aviso fixo no formulário: "Isto grava um handoff de verdade — a próxima sessão do projeto de destino
  recebe este conteúdo automaticamente."

## 5. Testes

- **Reuso, não duplicação:** um teste que a função de sanitização do handoff é **a mesma** chamada pelo
  MCP e pela web (por exemplo, comparando o `NewHandoff` produzido pelos dois caminhos com a mesma
  entrada) — para a paridade não quebrar num PR futuro que mexa só num dos dois lados.
- **CSRF, adversarial:** POST sem token → 403, nada gravado; POST com token de outra página (outro
  `from_path`) → 403; POST com token de 5 minutos atrás → 403; POST com `Origin` de outro host → 403;
  controle: POST com token válido e `Origin` correto → grava e redireciona. Confirma que morde: com a
  checagem de token removida, o teste de token ausente falha; restaurar.
- **Sanitização:** um resumo com `<script>` ou um segredo reconhecido pelo `Sanitizer` sai escapado/limpo
  no handoff gravado, igual ao caminho do MCP.
- **Escopo:** `from_workspace`/`from_project` do formulário são ignorados a favor do que o servidor
  preencheu ao renderizar a página (um `POST` forjado tentando declarar uma origem diferente da página não
  muda o dono real do handoff).
- **Projeto novo:** destino com nome que não existe cria o projeto, como o MCP; destino existente não
  duplica.
- **Admissão:** um webhook de admissão com política de recusa também recusa aqui (mesmo `AdmissionOp`).

## 6. Entrega

Um PR no fork, `alfama/feat-handoff-pela-tela`: a extração da Tarefa 1 (§4.1), a rota e o template, os
testes de CSRF e de sanitização, atualização do checklist de segurança (§2.1 da spec do painel, com a
exceção nomeada desta rota) e `docs/security-boundaries.md`. Imagem `2.4.1-alfama.N` e troca no servidor
com backup, como as demais telas.

## Verificação

1. Abrir uma página em `/w/default/aw-senai-sgw/p/decisions/0012-...`, expandir "Enviar como handoff",
   escolher (ou digitar) um projeto de destino, preencher resumo e enviar.
2. Conferir com `memory_handoff_list` (workspace/project de destino) que o handoff aparece, com o resumo e
   o link da página de origem no `next_steps`.
3. Editar o `curl` da mesma requisição sem o `csrf_token` → 403, nada aparece no `memory_handoff_list`.

## O que esta spec NÃO decide

- Se o handoff criado pela tela deveria também aparecer, de alguma forma, na tela "Entre projetos" antes
  de ser aceito — hoje ela só lista handoffs abertos, o que já cobre isso sem mudança.
- Editar ou cancelar um handoff pela tela (`memory_handoff_cancel`) fica para depois, se for pedido.
