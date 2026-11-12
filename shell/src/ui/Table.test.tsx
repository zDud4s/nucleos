import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { Table, Td, Th } from "./Table";

function sample() {
  return (
    <Table label="Spend by model">
      <thead>
        <tr>
          <Th>Model</Th>
          <Th num>Cost</Th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <Td muted>haiku</Td>
          <Td num>$1.20</Td>
        </tr>
      </tbody>
    </Table>
  );
}

describe("Table", () => {
  it("is a ui-table named by its label", () => {
    render(sample());
    const table = screen.getByRole("table", { name: "Spend by model" });
    expect(table.className).toBe("ui-table");
  });

  it("marks figure and muted cells, and leaves the rest bare", () => {
    render(sample());
    expect(screen.getByRole("columnheader", { name: "Cost" }).className).toBe("ui-table-num");
    expect(screen.getByRole("columnheader", { name: "Model" }).hasAttribute("class")).toBe(false);
    expect(screen.getByRole("cell", { name: "$1.20" }).className).toBe("ui-table-num");
    expect(screen.getByRole("cell", { name: "haiku" }).className).toBe("ui-table-muted");
  });

  it("heads columns by default and takes a row scope", () => {
    render(
      <Table label="t">
        <tbody>
          <tr>
            <Th scope="row">Total</Th>
            <Td num muted>
              3
            </Td>
          </tr>
        </tbody>
      </Table>,
    );
    expect(screen.getByRole("rowheader", { name: "Total" })).toBeTruthy();
    expect(screen.getByRole("cell", { name: "3" }).className).toBe("ui-table-num ui-table-muted");
  });

  it("names itself by its caption when it draws one", () => {
    render(
      <Table caption="Spend">
        <tbody />
      </Table>,
    );
    expect(screen.getByRole("table", { name: "Spend" })).toBeTruthy();
  });
});
