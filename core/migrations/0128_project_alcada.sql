-- Decisão #3. O que uma worktree deste projecto pode correr, e o que não pode em circunstância
-- nenhuma. NÃO substitui as listas compiladas: sobrepõe-se-lhes segundo a ordem da §4.2, onde um
-- `allow` daqui nunca levanta uma negação compilada nem as guardas de forma.
--
-- Sem linhas nenhumas, é como hoje: as listas compiladas decidem sozinhas.
CREATE TABLE project_shell_rules (
  id         INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  -- Medido como PREFIXO, a forma que `SAFE_COMMAND_PREFIXES` já usa e que
  -- `matches_command_prefix` já sabe comparar. Não é uma linha de shell: um prefixo que não passe
  -- `shell_form_is_readable` é recusado na escrita.
  prefix     TEXT NOT NULL CHECK (prefix <> ''),
  -- `allow` ou `deny`. `deny` ganha sempre — ver §4.2.
  --
  -- O CHECK é o argumento que o `0125_map_triage.sql` faz neste mesmo directório, aplicado a uma
  -- coluna da mesma forma: uma tabela que PODE guardar a palavra errada é uma tabela onde alguém
  -- acaba por a escrever. Aqui uma gralha em `deny` não dá erro nenhum — deixa cair, em silêncio,
  -- a recusa que alguém quis. A §6 já manda ignorar a linha malformada na leitura; isto impede-a
  -- de existir, e as duas coisas juntas são de propósito.
  verdict    TEXT NOT NULL CHECK (verdict IN ('allow', 'deny')),
  -- Porque é que isto está aqui, nas palavras de quem o escreveu. Opcional, e é a única defesa
  -- contra uma lista que daqui a seis meses ninguém sabe justificar.
  note       TEXT,
  created_at TEXT NOT NULL
);
-- Uma linha por prefixo e por projecto. `project_id` é a coluna à cabeça, portanto este índice
-- responde TAMBÉM a «todas as regras deste projecto», que é a única outra leitura que existe —
-- um segundo índice só sobre `project_id` não acrescentava caminho nenhum e custava escritas.
-- As três tabelas seguem esta mesma forma, e a simetria é o que se quer.
CREATE UNIQUE INDEX project_shell_rules_prefix ON project_shell_rules (project_id, prefix);

-- Decisão #4. As operações de GitHub que correm sem perguntar NESTE projecto. Uma linha por
-- operação, e `op_kind` é o nome tipado (`run_list`, `pr_comment`) e nunca um prefixo de shell —
-- é essa a unificação.
--
-- Sem linhas nenhumas, é como hoje: nada corre sozinho que o `.ai/github.yaml` não conceda.
CREATE TABLE project_github_ops (
  id         INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  op_kind    TEXT NOT NULL CHECK (op_kind <> ''),
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX project_github_ops_kind ON project_github_ops (project_id, op_kind);

-- Decisão #2. Os destinos que um `--land <branch>` pode nomear neste projecto.
--
-- O `integration_branch` é SEMPRE admissível, esteja ou não aqui. Uma tabela vazia não pode
-- significar «este projecto não aterra em lado nenhum» — significa «só no sítio do costume», que é
-- exactamente o comportamento de hoje.
CREATE TABLE project_land_targets (
  id         INTEGER PRIMARY KEY,
  project_id TEXT NOT NULL,
  branch     TEXT NOT NULL CHECK (branch <> ''),
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX project_land_targets_branch ON project_land_targets (project_id, branch);
