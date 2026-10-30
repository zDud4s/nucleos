import { useChatProject } from "../data/chats";

export const UNTRUSTED_NOTICE =
  "This tool reads third-party text: the end of the turn will rotate the session (with summary).";

/**
 * A non-blocking note shown once the chat's session has called a tool that reads third-party text.
 * Only a chat with a root directory and a live session rotates, so any other chat shows nothing.
 * It never asks for anything: there is nothing to answer.
 */
export function UntrustedNotice({
  chatId,
  firstCall,
}: {
  chatId: string;
  firstCall: { tool: string } | null | undefined;
}) {
  const project = useChatProject(chatId).data;
  if (!firstCall || !project || project.cwd === null || project.session === null) return null;
  return (
    <p className="chats-hint" role="note">
      {UNTRUSTED_NOTICE}
    </p>
  );
}
