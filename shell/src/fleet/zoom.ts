import type { ReactFlowState } from "@xyflow/react";
import { CARD_ZOOM } from "./limits";

/**
 * A boolean selector, so a component re-renders when the zoom crosses the card threshold and not
 * on every wheel tick between. Shared by the node (to draw a card) and the canvas (to lay out
 * card-sized rows).
 */
export const selectCard = (state: ReactFlowState): boolean => state.transform[2] < CARD_ZOOM;
