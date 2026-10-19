// §spec mapa-do-projeto
import { useEffect, useMemo, useState } from "react";
import { useAcceptLayers, useSetLayer, type Layering, type ProjectMap } from "../data/project-map";
import { buildBands, isolate, ladder, maxLayer, type Band, type LayerBlock } from "./layers-model";
import { DAEMON_NAME } from "../copy";

/**
 * The modules stacked by layer, highest first: what may import what, and which imports break it.
 *
 * Clicking a band isolates it, and Escape goes back to the whole stack. Choosing a block lists its
 * files, and each file can be moved to another layer by hand; the move is the owner's and survives
 * a re-read of the map.
 */
export function AtlasLayers({ projectId, data }: { projectId: string; data: ProjectMap }) {
  const layering = data.layers;
  if (layering === undefined) {
    return <p className="text-sm text-text-muted">This {DAEMON_NAME} does not report layers.</p>;
  }
  return <Stack projectId={projectId} data={data} layering={layering} />;
}

function Stack({
  projectId,
  data,
  layering,
}: {
  projectId: string;
  data: ProjectMap;
  layering: Layering;
}) {
  const accept = useAcceptLayers(projectId);
  const [isolated, setIsolated] = useState<number | null>(null);
  const [chosen, setChosen] = useState<{ layer: number; key: string } | null>(null);
  const bands = useMemo(() => buildBands(data.modules, layering), [data.modules, layering]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setIsolated(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const view = isolated === null ? null : isolate(bands, data.imports, layering, isolated);
  const shown: Band[] = view === null ? bands : [view.band];
  const upward = layering.violations.length;
  const upwardNew = layering.violations.filter((violation) => violation.new).length;
  const block =
    chosen === null
      ? undefined
      : shown
          .flatMap((band) => band.blocks)
          .find((b) => b.layer === chosen.layer && b.key === chosen.key);

  return (
    <div className="ws-layers">
      <p className="m-0 text-sm text-text-muted">
        <span className={upward > 0 ? "ws-layer-up" : undefined}>
          {upward} upward imports ({upwardNew} new)
        </span>
        {upwardNew > 0 ? (
          <button
            type="button"
            className="ml-2"
            disabled={accept.isPending}
            onClick={() => accept.mutate()}
          >
            Accept today&apos;s graph
          </button>
        ) : null}
      </p>

      {upward === 0 ? null : <LayerLines bands={bands} layering={layering} />}

      {shown.map((band) => (
        <section key={band.layer} className="ws-layer-band">
          <button
            type="button"
            aria-pressed={isolated === band.layer}
            onClick={() => setIsolated(isolated === band.layer ? null : band.layer)}
          >
            {`Layer ${band.layer} · ${band.files} files · ↑${band.upward}`}
          </button>
          <div className="flex flex-wrap gap-2">
            {band.blocks.map((b) => (
              <button
                key={b.key}
                type="button"
                className="ws-layer-block"
                aria-pressed={chosen?.layer === b.layer && chosen.key === b.key}
                onClick={() =>
                  setChosen(
                    chosen?.layer === b.layer && chosen.key === b.key
                      ? null
                      : { layer: b.layer, key: b.key },
                  )
                }
              >
                <span>{b.label}</span>
                <span>{b.files.length}</span>
                {b.upward > 0 ? <span className="ws-layer-up">{`↑${b.upward}`}</span> : null}
              </button>
            ))}
          </div>
        </section>
      ))}

      {view === null ? null : (
        <p className="m-0 text-sm text-text-muted">
          {`In from ${describe(view.into)}; out to ${describe(view.outOf)}.`}
        </p>
      )}

      {block === undefined ? null : (
        <BlockFiles projectId={projectId} block={block} layering={layering} />
      )}

      {upward === 0 ? null : (
        <ul className="m-0 flex list-none flex-col gap-1 p-0 text-sm">
          {layering.violations.map((violation) => (
            <li key={`${violation.from}\0${violation.to}`} className="ws-layer-up">
              {`${violation.from} → ${violation.to}, ${violation.from_layer} → ${violation.to_layer}`}
              {violation.new ? " · new" : ""}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const ROW = 22;
const LABEL = 150;
const WIDTH = 260;

/**
 * Every upward import drawn as a line between the rows of the layers it joins, danger-coloured
 * when it is new. A deep stack folds its quiet layers into one row (see {@link ladder}).
 */
function LayerLines({ bands, layering }: { bands: Band[]; layering: Layering }) {
  const { rows, lines } = useMemo(
    () => ladder(bands, layering.violations),
    [bands, layering.violations],
  );
  const y = (row: number) => row * ROW + ROW / 2;
  return (
    <svg
      role="img"
      aria-label="Upward imports between layers"
      width={WIDTH}
      height={rows.length * ROW}
      className="ws-layer-lines"
    >
      {rows.map((row, index) => (
        <text key={`${row.top}-${row.bottom}`} x={0} y={y(index) + 4} fontSize={11}>
          {row.top === row.bottom
            ? `Layer ${row.top}`
            : `Layers ${row.top}–${row.bottom} · ${row.files} files`}
        </text>
      ))}
      {lines.map((line) => {
        const bulge = LABEL + 12 * Math.abs(line.fromRow - line.toRow);
        const from = y(line.fromRow);
        const to = y(line.toRow);
        return (
          <path
            key={`${line.fromRow}-${line.toRow}`}
            className="ws-layer-line"
            data-new={line.newCount > 0 ? "true" : "false"}
            d={`M ${LABEL} ${from} C ${bulge} ${from}, ${bulge} ${to}, ${LABEL} ${to}`}
            fill="none"
            strokeWidth={1 + Math.min(line.count, 4)}
          >
            <title>{`${line.count} upward imports, ${line.newCount} new`}</title>
          </path>
        );
      })}
    </svg>
  );
}

/** "layer 2 (3), layer 0 (1)", or "nowhere" for an edge with nothing crossing it. */
function describe(counts: Map<number, number>): string {
  if (counts.size === 0) return "nowhere";
  return [...counts.entries()]
    .sort((a, b) => b[0] - a[0])
    .map(([layer, count]) => `layer ${layer} (${count})`)
    .join(", ");
}

function BlockFiles({
  projectId,
  block,
  layering,
}: {
  projectId: string;
  block: LayerBlock;
  layering: Layering;
}) {
  const setLayer = useSetLayer(projectId);
  const choices = Array.from({ length: maxLayer(layering) + 2 }, (_, layer) => layer);
  return (
    <ul className="m-0 flex list-none flex-col gap-1 p-0 text-sm">
      {block.files.map((path) => (
        <li key={path} className="flex items-center gap-2">
          <span>{path}</span>
          <select
            aria-label={`Layer of ${path}`}
            value={String(layering.modules[path] ?? block.layer)}
            onChange={(event) => setLayer.mutate({ path, layer: Number(event.target.value) })}
          >
            {choices.map((layer) => (
              <option key={layer} value={String(layer)}>
                {layer}
              </option>
            ))}
          </select>
          {layering.overridden.includes(path) ? (
            <button type="button" onClick={() => setLayer.mutate({ path, layer: null })}>
              reset
            </button>
          ) : null}
        </li>
      ))}
    </ul>
  );
}
