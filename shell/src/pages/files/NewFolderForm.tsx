import { useState } from "react";
import { joinPath, useCreateFolder } from "../../data/files";
import { Button, Field } from "../../ui";
import { CREATE_FOLDER_SENTENCES, RefusalOrError } from "./shared";

/* ----------------------------------------------------------- new folder -- */

/**
 * A button until it is wanted. Making a folder is occasional, and an open
 * field beside the search took half the toolbar for it; opened, the field
 * takes focus, Escape puts it away, and the refusal lands under the field that
 * caused it rather than at the foot of the page.
 */
export function NewFolderForm({ path, create }: { path: string; create: ReturnType<typeof useCreateFolder> }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");

  function close() {
    setOpen(false);
    setName("");
    create.reset();
  }

  if (!open) {
    return (
      <Button
        variant="ghost"
        onClick={() => {
          create.reset();
          setOpen(true);
        }}
      >
        New folder
      </Button>
    );
  }

  return (
    <form
      className="fi-new-folder"
      onSubmit={(event) => {
        event.preventDefault();
        const trimmed = name.trim();
        if (trimmed === "" || create.isPending) return;
        create.mutate(joinPath(path, trimmed), { onSuccess: close });
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") close();
      }}
    >
      <div className="fi-new-folder-row">
        <Field label="New folder name">
          <input
            // eslint-disable-next-line jsx-a11y/no-autofocus -- the field was
            // opened by the button that just vanished; focus belongs here.
            autoFocus
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </Field>
        <Button type="submit" disabled={name.trim() === "" || create.isPending}>
          Create folder
        </Button>
        <Button variant="ghost" onClick={close}>
          Cancel
        </Button>
      </div>
      {create.isError && (
        <RefusalOrError error={create.error} sentences={CREATE_FOLDER_SENTENCES} what="that folder was not created" />
      )}
    </form>
  );
}
