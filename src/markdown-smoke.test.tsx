import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeSanitize from "rehype-sanitize";

describe("ReactMarkdown", () => {
  it("renders bold markdown content", () => {
    render(
      <ReactMarkdown remarkPlugins={[remarkGfm]} rehypePlugins={[rehypeSanitize]}>
        {"**bold**"}
      </ReactMarkdown>,
    );

    const boldElement = screen.getByText("bold");
    expect(boldElement).toBeInTheDocument();
    expect(boldElement.tagName).toBe("STRONG");
  });
});
