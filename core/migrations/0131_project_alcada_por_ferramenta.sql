-- A segunda dimensão de `project_shell_rules` (0128): a FERRAMENTA a que a regra se aplica.
--
-- Até aqui uma linha desta tabela nomeava um prefixo e mais nada, e o prefixo era sempre um
-- prefixo de COMANDO. A partir daqui a mesma tabela guarda duas espécies de regra, e é a coluna
-- `tool` que diz qual:
--   `''`               -- regra de shell, exactamente a de 0128. O prefixo é um prefixo de linha
--                         de comando.
--   `'Edit'`/`'Write'` -- regra de escrita. O prefixo é um CAMINHO, e a regra diz que esta
--                         ferramenta não escreve por baixo dele neste projecto.
--
-- Sem linhas com `tool` preenchido, é como hoje: a coluna vale `''` em todas as linhas que já cá
-- estavam e nenhuma decisão muda.
--
-- O CHECK é o argumento que 0128 já faz sobre `verdict`, aplicado a uma coluna da mesma espécie:
-- uma tabela que PODE guardar o nome errado é uma tabela onde alguém acaba por o escrever, e um
-- `'edit'` em minúsculas seria uma restrição que nunca se aplica a ferramenta nenhuma — uma recusa
-- que em silêncio não é uma recusa. A lista é fechada porque o conjunto de ferramentas que
-- escrevem ficheiros é fechado no código que a lê.
ALTER TABLE project_shell_rules ADD COLUMN tool TEXT NOT NULL DEFAULT ''
  CHECK (tool IN ('', 'Edit', 'Write'));

-- `''` e nunca NULL, e esta é a razão pela qual a coluna é `NOT NULL DEFAULT ''` em vez da coluna
-- anulável que a leitura do código sugeriria (a ausência de ferramenta é mesmo uma ausência).
--
-- Num índice UNIQUE o SQLite trata cada NULL como DISTINTO de todos os outros. Com `tool` anulável,
-- `(project_id, tool, prefix)` deixaria de ter uma linha por prefixo de shell: duas linhas com o
-- mesmo `project_id` e o mesmo `prefix` e `tool` NULL nas duas não colidem, o `ON CONFLICT` do
-- INSERT nunca dispara, e quem mudasse de ideias sobre um veredicto ficaria com AS DUAS respostas
-- na tabela — que é exactamente o defeito que o índice de 0128 existe para impedir. `''` é um
-- valor como qualquer outro para o índice, e a distinção «isto é uma regra de shell» fica onde
-- pertence, na leitura, e não na semântica de comparação do motor.
DROP INDEX project_shell_rules_prefix;

-- Uma linha por (projecto, ferramenta, prefixo). `project_id` continua à cabeça pela razão que
-- 0128 dá — este índice responde TAMBÉM a «todas as regras deste projecto», que é a única outra
-- leitura que existe. `tool` vem antes de `prefix` porque a leitura ordena por `tool, prefix`, e
-- porque assim as regras de shell (`tool = ''`) ficam contíguas: são as que a decisão de uma linha
-- de comando percorre.
CREATE UNIQUE INDEX project_shell_rules_scope ON project_shell_rules (project_id, tool, prefix);
