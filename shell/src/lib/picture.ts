/**
 * Turning a file the browser handed us into a picture the daemon can send.
 *
 * Kept apart from the page for the reason `mention.ts` is: what counts as a picture, and how a
 * `File` becomes base64, are decisions about *data*, and a module that cannot import a component is
 * a module that cannot grow one.
 */

import type { Attachment } from "../data/chats";

/**
 * What the daemon will actually send, and therefore what the window offers.
 *
 * The API takes these four and nothing else. A `.bmp` dropped on the box is refused here rather
 * than accepted, uploaded, and refused at the far end after the person has waited for it.
 */
const SENDABLE = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/** Whether this is a picture the daemon can carry. */
export function isPicture(file: File): boolean {
  return SENDABLE.includes(file.type);
}

/**
 * The media type and base64 payload out of a `data:` URL.
 *
 * Its own function because it is the one part of reading a file that can be wrong in a way nothing
 * would notice: a payload still carrying its `data:image/png;base64,` prefix is valid base64 of the
 * wrong bytes, so it reaches the model as a picture that will not decode rather than as an error.
 */
export function splitDataUrl(url: string): Attachment | null {
  const match = /^data:([^;,]+);base64,(.*)$/s.exec(url);
  if (match === null) return null;
  return { media_type: match[1], data: match[2] };
}

/**
 * Reads a picture into the shape the daemon takes.
 *
 * `readAsDataURL` rather than `readAsArrayBuffer` and a hand-rolled encoder: the browser has to
 * base64 it either way, and doing it here would be the same work done twice, once worse.
 */
export function attachmentFrom(file: File): Promise<Attachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error(`could not read ${file.name}`));
    reader.onload = () => {
      const split = typeof reader.result === "string" ? splitDataUrl(reader.result) : null;
      // The type the FILE claims wins over the one in the data URL when they differ: it is what
      // `isPicture` was asked about, so letting the other one through would send something the
      // window never agreed to.
      if (split === null) reject(new Error(`could not read ${file.name}`));
      else resolve({ media_type: file.type, data: split.data });
    };
    reader.readAsDataURL(file);
  });
}
