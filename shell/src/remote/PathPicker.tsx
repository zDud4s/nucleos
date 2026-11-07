import { useEffect, useRef, useState } from "react";
import { apiFetch } from "../data/client";
import { Button, ErrorNote, Modal } from "../ui";
import { fetchSession } from "./session";

interface DirectoryListing {
  path: string;
  parent: string | null;
  dirs: string[];
  truncated: boolean;
}

interface PathPickerProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onSelect: (path: string) => void;
  initialPath?: string;
}

/** Browse folders on the tower when the native desktop picker is not available in a browser. */
export function PathPicker({ open, onOpenChange, onSelect, initialPath = "" }: PathPickerProps) {
  const [listing, setListing] = useState<DirectoryListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [owner, setOwner] = useState<boolean | null>(null);
  const requestCounter = useRef(0);

  useEffect(() => {
    if (!open) return undefined;
    let current = true;
    requestCounter.current += 1;
    setListing(null);
    setError(null);
    setOwner(null);

    async function begin() {
      const session = await fetchSession();
      if (!current) return;
      if (session.kind !== "ok") {
        setError("The session could not be checked.");
        return;
      }
      if (!session.session.owner) {
        setOwner(false);
        return;
      }
      setOwner(true);
      const loaded = await load(initialPath);
      if (current && !loaded && initialPath !== "") {
        await load("", "That path could not be opened. Showing the tower root instead.");
      }
    }

    async function load(path: string, successError?: string): Promise<boolean> {
      const request = ++requestCounter.current;
      setLoading(true);
      setError(null);
      try {
        const next = await apiFetch<DirectoryListing>(`/fs/dirs?path=${encodeURIComponent(path)}`);
        if (current && request === requestCounter.current) {
          setListing(next);
          setError(successError ?? null);
          return true;
        }
      } catch (cause) {
        if (current && request === requestCounter.current) {
          setError(cause instanceof Error ? cause.message : "The tower could not list this folder.");
          return false;
        }
      } finally {
        if (current && request === requestCounter.current) setLoading(false);
      }
      return false;
    }

    void begin();
    return () => {
      current = false;
      requestCounter.current += 1;
    };
  }, [initialPath, open]);

  async function browse(path: string) {
    const request = ++requestCounter.current;
    setLoading(true);
    setError(null);
    try {
      const next = await apiFetch<DirectoryListing>(`/fs/dirs?path=${encodeURIComponent(path)}`);
      if (request === requestCounter.current) setListing(next);
    } catch (cause) {
      if (request === requestCounter.current) {
        setError(cause instanceof Error ? cause.message : "The tower could not list this folder.");
      }
    } finally {
      if (request === requestCounter.current) setLoading(false);
    }
  }

  const selectedPath = listing?.path ?? "";
  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="Choose a folder on the tower"
      description="This browser can browse folders on the machine running the daemon."
      size="md"
      footer={
        <>
          <Button type="button" variant="quiet" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            type="button"
            variant="primary"
            disabled={loading || listing === null || selectedPath === ""}
            onClick={() => onSelect(selectedPath)}
          >
            Select
          </Button>
        </>
      }
    >
      {owner === false ? (
        <p>Browsing folders on the tower needs an owner device; you can still type the path by hand.</p>
      ) : (
        <div className="flex flex-col gap-3">
          <p className="font-mono text-sm text-text" aria-live="polite">
            {(listing?.path ?? initialPath) || "Loading the tower root…"}
          </p>
          <div className="flex items-center gap-2">
            <Button
              type="button"
              disabled={loading || listing?.parent === null || listing === null}
              onClick={() => void browse(listing?.parent ?? "")}
            >
              Up
            </Button>
            {loading ? <span role="status">Loading folders…</span> : null}
          </div>
          {error === null ? null : <ErrorNote>{error}</ErrorNote>}
          {listing !== null && !loading && listing.dirs.length === 0 ? <p>No folders are here.</p> : null}
          {listing !== null && !loading && listing.dirs.length > 0 ? (
            <div className="flex max-h-80 flex-col overflow-auto rounded-sm border border-border">
              {listing.dirs.map((name) => (
                <Button key={name} type="button" variant="quiet" onClick={() => void browse(joinPath(listing.path, name))}>
                  {name}
                </Button>
              ))}
            </div>
          ) : null}
          {listing?.truncated === true ? <p>Only the first 2,000 folders are shown.</p> : null}
        </div>
      )}
    </Modal>
  );
}

function joinPath(path: string, name: string): string {
  if (path === "" || path.endsWith("/") || path.endsWith("\\")) return `${path}${name}`;
  return `${path}${path.includes("\\") ? "\\" : "/"}${name}`;
}
