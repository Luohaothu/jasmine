import React from "react";
import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import styles from "./RichTextRenderer.module.css";

interface RichTextRendererProps {
  content: string;
}

const remarkMentions = () => {
  return (tree: any) => {
    const walk = (node: any) => {
      if (!node.children) return;
      let prev = null;
      for (const child of node.children) {
        if (
          child.type === "link" &&
          child.url &&
          child.url.startsWith("user:") &&
          prev &&
          prev.type === "text" &&
          prev.value.endsWith("@")
        ) {
          prev.value = prev.value.slice(0, -1);
        }
        prev = child;
        walk(child);
      }
    };
    walk(tree);
  };
};

const remarkHtmlBlockFix = () => {
  return (tree: any) => {
    const walk = (node: any, parent: any) => {
      // Find block-level HTML nodes (those not in phrasing content like paragraphs)
      if (node.type === "html" && parent && parent.type !== "paragraph" && parent.type !== "heading") {
        // Strip script and style tags completely, including their content
        let safeValue = node.value
          .replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/gi, "")
          .replace(/<(script|style)\b[^>]*\/>/gi, "")
          .replace(/<\/?(script|style)\b[^>]*>/gi, "");

        if (!safeValue.trim()) {
          node.type = "text";
          node.value = "";
          return;
        }

        // Convert the remaining block HTML into a paragraph with inline HTML and text
        // This prevents the renderer from silently swallowing safe trailing text
        node.type = "paragraph";
        node.children = [];
        const parts = safeValue.split(/(<[^>]+>)/g);
        for (const part of parts) {
          if (!part) continue;
          if (part.startsWith("<") && part.endsWith(">")) {
            node.children.push({ type: "html", value: part });
          } else {
            node.children.push({ type: "text", value: part });
          }
        }
      } else if (node.children) {
        for (const child of node.children) {
          walk(child, node);
        }
      }
    };
    walk(tree, null);
  };
};

const customUrlTransform = (url: string) => {
  if (url.startsWith("user:")) return url;
  return defaultUrlTransform(url);
};

const customSchema = {
  ...defaultSchema,
  tagNames: defaultSchema.tagNames?.filter(
    (tag) => !["table", "thead", "tbody", "tr", "th", "td", "input", "h1", "h2", "h3", "h4", "h5", "h6", "blockquote"].includes(tag)
  ),
  attributes: {
    ...defaultSchema.attributes,
    li: defaultSchema.attributes?.li?.filter((attr) => attr !== "className" && attr !== "class") || [],
    ul: defaultSchema.attributes?.ul?.filter((attr) => attr !== "className" && attr !== "class") || [],
    ol: defaultSchema.attributes?.ol?.filter((attr) => attr !== "className" && attr !== "class") || [],
  },
  protocols: {
    ...defaultSchema.protocols,
    href: [...(defaultSchema.protocols?.href || []), "user"],
  },
};

export const RichTextRenderer: React.FC<RichTextRendererProps> = ({ content }) => {
  return (
    <div className={styles.container}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkMentions, remarkHtmlBlockFix]}
        rehypePlugins={[[rehypeSanitize, customSchema]]}
        urlTransform={customUrlTransform}
        allowedElements={["p", "span", "strong", "em", "del", "code", "pre", "a", "ul", "ol", "li", "br"]}
        unwrapDisallowed={true}
        components={{
          a: ({ href, children }) => {
            if (href && href.startsWith("user:")) {
              const isEmptyMention =
                !children ||
                (Array.isArray(children) && children.length === 0) ||
                (typeof children === "string" && !children.trim()) ||
                href === "user:";

              if (isEmptyMention) {
                return (
                  <a href={href} target="_blank" rel="noopener noreferrer">
                    {children}
                  </a>
                );
              }

              return (
                <span className={styles.mention} data-mention={href}>
                  @{children}
                </span>
              );
            }
            return (
              <a href={href} target="_blank" rel="noopener noreferrer" className={styles.linkCard} data-testid="link-card">
                <span className={styles.linkCardIcon}>🔗</span>
                <span className={styles.linkCardContent}>
                  <span className={styles.linkCardTitle}>{children}</span>
                  <span className={styles.linkCardUrl}>{href}</span>
                </span>
              </a>
            );
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
};
