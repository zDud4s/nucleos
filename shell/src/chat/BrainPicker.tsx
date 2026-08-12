import type { Brain } from "../api";

interface BrainPickerProps {
  brain: Brain;
  /**
   * Whether this machine has a local model at all.
   *
   * Asked of the daemon rather than assumed, so the picker can refuse the switch here instead of
   * taking it and leaving the conversation set to a model that cannot answer it.
   */
  localAvailable: boolean;
  /** Whether a turn is in flight for this conversation. */
  busy: boolean;
  onChange: (brain: Brain) => void;
}

const OPTIONS: { value: Brain; label: string }[] = [
  { value: "cloud", label: "cloud" },
  { value: "local", label: "local" },
];

/**
 * Which model answers this conversation.
 *
 * Two radio buttons rather than a toggle, because the reason each is unavailable differs and a
 * toggle has nowhere to say so. The daemon refuses a switch under a live turn with a 409 — which
 * model answered is written when a turn's row is born, so moving it mid-turn would make that record
 * lie — and this asks the same question before sending, so the refusal is explained rather than met.
 */
function BrainPicker({ brain, localAvailable, busy, onChange }: BrainPickerProps) {
  return (
    <div className="brain-picker">
      <span className="bp-label">answered by</span>
      {OPTIONS.map((option) => {
        const unavailable = option.value === "local" && !localAvailable;
        return (
          <label key={option.value} className="bp-option">
            <input
              type="radio"
              name="brain"
              value={option.value}
              checked={brain === option.value}
              disabled={busy || unavailable}
              // Refused here as well as by `disabled`, and not only for tidiness: `disabled` is
              // enforced by the browser's activation behaviour, so anything dispatching the event
              // another way — a script, a test, a future keyboard handler — would walk straight
              // past it into a switch the daemon is about to answer 409 to.
              onChange={() => {
                if (busy || unavailable) return;
                onChange(option.value);
              }}
            />
            {option.label}
          </label>
        );
      })}
      {!localAvailable && (
        <span className="a-note">No local model is configured on this machine.</span>
      )}
      {busy && <span className="a-note">The model cannot change while a turn is in flight.</span>}
      {/*
        Said before the switch, not after. The two directions differ — the local model re-reads the
        recent turns out of the database, the cloud starts on a session that was just dropped — and
        the transcript marks which of the two happened where. What is common to both, and the only
        thing worth warning about here, is that the thread's memory starts again.
      */}
      {localAvailable && !busy && (
        <span className="a-note">Changing this means the conversation&apos;s memory starts again.</span>
      )}
    </div>
  );
}

export default BrainPicker;
