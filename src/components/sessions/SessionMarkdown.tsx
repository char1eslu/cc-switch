import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import { cn } from "@/lib/utils";

const remarkPlugins = [remarkGfm];

const headingClassName = (level: 1 | 2 | 3 | 4 | 5 | 6) =>
  cn(
    "mb-1.5 mt-3 font-semibold leading-snug first:mt-0",
    level <= 2 ? "text-[0.98rem]" : "text-sm",
  );

const components: Components = {
  h1: ({ children }) => <h1 className={headingClassName(1)}>{children}</h1>,
  h2: ({ children }) => <h2 className={headingClassName(2)}>{children}</h2>,
  h3: ({ children }) => <h3 className={headingClassName(3)}>{children}</h3>,
  h4: ({ children }) => <h4 className={headingClassName(4)}>{children}</h4>,
  h5: ({ children }) => <h5 className={headingClassName(5)}>{children}</h5>,
  h6: ({ children }) => <h6 className={headingClassName(6)}>{children}</h6>,
  p: ({ children }) => <p className="mb-2 last:mb-0">{children}</p>,
  a: ({ children, href }) => (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="font-medium text-primary underline underline-offset-4"
    >
      {children}
    </a>
  ),
  ul: ({ children }) => (
    <ul className="my-2 list-disc space-y-1 pl-5">{children}</ul>
  ),
  ol: ({ children }) => (
    <ol className="my-2 list-decimal space-y-1 pl-5">{children}</ol>
  ),
  li: ({ children }) => <li className="pl-1">{children}</li>,
  blockquote: ({ children }) => (
    <blockquote className="my-2 border-l-2 border-border pl-3 text-muted-foreground">
      {children}
    </blockquote>
  ),
  hr: () => <hr className="my-3 border-border" />,
  table: ({ children }) => (
    <div className="my-2 overflow-x-auto rounded-md border">
      <table className="min-w-full border-collapse text-xs">{children}</table>
    </div>
  ),
  thead: ({ children }) => <thead className="bg-muted/70">{children}</thead>,
  th: ({ children }) => (
    <th className="border-b px-2 py-1.5 text-left font-semibold">{children}</th>
  ),
  td: ({ children }) => (
    <td className="border-t px-2 py-1.5 align-top">{children}</td>
  ),
  pre: ({ children }) => (
    <pre className="my-2 max-w-full overflow-x-auto rounded-md bg-muted/70 p-0">
      {children}
    </pre>
  ),
  code: ({ children, className }) => {
    const text = String(children);
    const isBlock = text.includes("\n") || Boolean(className);

    if (isBlock) {
      return (
        <code
          className={cn(
            "block min-w-0 px-3 py-2 font-mono text-xs leading-relaxed",
            className,
          )}
        >
          {children}
        </code>
      );
    }

    return (
      <code className="rounded bg-muted px-1 py-0.5 font-mono text-[0.92em]">
        {children}
      </code>
    );
  },
};

interface SessionMarkdownProps {
  content: string;
}

export function SessionMarkdown({ content }: SessionMarkdownProps) {
  return (
    <div className="min-w-0 break-words text-sm leading-relaxed [overflow-wrap:anywhere]">
      <ReactMarkdown remarkPlugins={remarkPlugins} components={components}>
        {content}
      </ReactMarkdown>
    </div>
  );
}
