/** The alphabet of an item's state. The daemon sends `done`, `running`, `pending` and `failed`. */
export const MARK: Record<string, string> = { done: "✓", running: "⋯", pending: "·", failed: "✗" };
