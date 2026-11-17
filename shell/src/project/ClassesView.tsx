// §spec atlas-classes
import { useState } from "react";
import { DAEMON_NAME } from "../copy";
import { useFileItems, type FileItems, type ItemMember } from "../data/project-map";
import { ErrorNote, Select } from "../ui";

/**
 * The shapes one Rust file declares, drawn as boxes: a struct lists its fields, an enum its
 * variants, a trait stands empty. Relations are the in-file references between those boxes, and
 * the `use` paths say what the file leans on. What the reader could not see is said under it.
 */
export function ClassesView({ projectId, modules }: { projectId: string; modules: string[] }) {
  const rust = modules.filter((path) => path.endsWith(".rs"));
  const [picked, setPicked] = useState<string | null>(null);
  const path = picked !== null && rust.includes(picked) ? picked : (rust[0] ?? null);
  const items = useFileItems(projectId, path);

  if (path === null) {
    return (
      <p
        className="m-0 text-sm text-text-muted"
        title="This project has no Rust file to draw classes from."
      >
        No Rust file
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <label className="flex items-baseline gap-2 text-sm">
        File
        <Select aria-label="File" value={path} onChange={(event) => setPicked(event.target.value)}>
          {rust.map((candidate) => (
            <option key={candidate} value={candidate}>
              {candidate}
            </option>
          ))}
        </Select>
      </label>
      {items.isError && items.data === undefined ? (
        <ErrorNote>
          The {DAEMON_NAME} could not read the items of {path}.
        </ErrorNote>
      ) : items.data === undefined ? (
        <p className="text-sm text-text-faint">Reading {path}…</p>
      ) : (
        <Diagram file={items.data} />
      )}
    </div>
  );
}

function Diagram({ file }: { file: FileItems }) {
  const boxes = file.items.filter(
    (item) => item.container === null && (item.kind === "shape" || item.kind === "contract"),
  );
  const ids = new Set(boxes.map((box) => box.id));
  const relations = file.references.filter(
    (reference) =>
      ids.has(reference.from) && ids.has(reference.to) && reference.from !== reference.to,
  );
  const byOwner = new Map<string, ItemMember[]>();
  const uses = file.uses ?? [];
  for (const member of file.members ?? []) {
    byOwner.set(member.owner, [...(byOwner.get(member.owner) ?? []), member]);
  }
  return (
    <div className="flex flex-col gap-4">
      {boxes.length === 0 ? (
        <p className="m-0 text-sm text-text-muted">This file declares no struct, enum or trait.</p>
      ) : (
        <div className="flex flex-wrap gap-3">
          {boxes.map((box) => (
            <section
              key={box.id}
              aria-label={box.name}
              className="min-w-40 rounded border border-border p-2 text-sm"
            >
              <h3 className="m-0 text-sm">
                {box.name}
                <span className="ml-2 text-text-faint">
                  {box.kind === "contract" ? "trait" : box.exported ? "pub" : "private"}
                </span>
              </h3>
              <ul className="m-0 mt-1 list-none p-0">
                {(byOwner.get(box.id) ?? []).map((member) => (
                  <li key={`${member.name}:${member.line}`} className="flex gap-2">
                    <span>{member.name}</span>
                    {member.ty === null ? null : (
                      <span className="text-text-faint">{member.ty}</span>
                    )}
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>
      )}
      {relations.length === 0 ? null : (
        <section aria-label="Relations">
          <h3 className="m-0 text-sm">Relations</h3>
          <ul className="m-0 list-none p-0 text-sm">
            {relations.map((relation) => (
              <li key={`${relation.from}>${relation.to}`}>
                {`${relation.from} → ${relation.to}`}
              </li>
            ))}
          </ul>
        </section>
      )}
      {uses.length === 0 ? null : (
        <section aria-label="Uses">
          <h3 className="m-0 text-sm">Uses</h3>
          <ul className="m-0 list-none p-0 text-sm">
            {uses.map((use) => (
              <li key={`${use.path}:${use.line}`}>{use.path}</li>
            ))}
          </ul>
        </section>
      )}
      {file.missed.length === 0 ? null : (
        <p className="m-0 text-sm text-text-faint">
          {`Not seen by the reader: ${file.missed.join("; ")}`}
        </p>
      )}
    </div>
  );
}
