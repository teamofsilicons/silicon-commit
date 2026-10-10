"use client";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import s from "./product.module.css";
/** Raw HTML and remote media stay inert; Markdown links use the renderer's safe URL transform. */
export function Markdown({text}:{text:string}) {return <div className={s.markdown}><ReactMarkdown remarkPlugins={[remarkGfm]} skipHtml disallowedElements={["img","input"]} components={{a:({href,children})=><a href={href} target="_blank" rel="noreferrer">{children}</a>}}>{text}</ReactMarkdown></div>;}
