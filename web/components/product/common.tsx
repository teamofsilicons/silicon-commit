"use client";
import {useState, type ReactNode} from "react";
import {useInfiniteQuery, useQuery, useQueryClient} from "@tanstack/react-query";
import {Button} from "@/components/silicon-ui/button/button";
import {Select} from "@/components/silicon-ui/select/select";
import {Dialog,DialogContent} from "@/components/silicon-ui/dialog/dialog";
import {AccountChip} from "@/components/foundation/account/account-chip";
import {ErrorAlert} from "@/components/foundation/feedback/error-alert";
import {ApiError} from "@/lib/errors";
import {request,type RequestOptions} from "@/lib/client/api";
import type {Actor,PageResult} from "./types";
import styles from "./product.module.css";
export const enc=encodeURIComponent;
export const lines=(text:string)=>[...new Set(text.split(/[\n,]/).map(x=>x.trim()).filter(Boolean))];
export const label=(text:string)=>text.replaceAll("_"," ").replace(/^./,c=>c.toUpperCase());
export const stamp=(text:string)=>new Date(text).toLocaleString();
export const call=request;
const pending=new Map<string,string>();
export async function mutate<T>(path:string,method:RequestOptions["method"],body?:unknown,version?:number):Promise<T>{
 const fingerprint=JSON.stringify([path,method,body,version]);
 const key=pending.get(fingerprint)??crypto.randomUUID();pending.set(fingerprint,key);
 const result=await request<T>(path,{method,body,idempotencyKey:key,headers:version===undefined?undefined:{"If-Match":`"${version}"`}});pending.delete(fingerprint);return result;
}
export function useResource<T>(path:string,initialData?:T){return useQuery({queryKey:[path],queryFn:({signal})=>call<T>(path,{signal}),initialData});}
export function useList<T>(path:string,initialData?:PageResult<T>){const query=useInfiniteQuery({queryKey:[path],queryFn:({signal,pageParam})=>call<PageResult<T>>(path+(pageParam?`${path.includes("?")?"&":"?"}cursor=${enc(pageParam)}`:""),{signal}),initialPageParam:"",getNextPageParam:(page)=>page.next_cursor||undefined,initialData:initialData?{pages:[initialData],pageParams:[""]}:undefined});return {...query,items:query.data?.pages.flatMap(p=>p.items)??[]};}
export function useAction(){const cache=useQueryClient();const [error,setError]=useState<unknown>(null);const [busy,setBusy]=useState(false);const [success,setSuccess]=useState("");return {error,busy,success,reset:()=>{setError(null);setSuccess("");},run:async(fn:()=>Promise<unknown>,message="Saved")=>{setError(null);setSuccess("");setBusy(true);try{await fn();await cache.invalidateQueries();setSuccess(message);return true;}catch(e){setError(e);return false;}finally{setBusy(false);}}};}
export function Feedback({action}:{action:ReturnType<typeof useAction>}){return <>{action.error?<ErrorAlert error={ApiError.from(action.error)}/>:null}{action.success?<p role="status" className={styles.notice}>{action.success}</p>:null}</>;}
export function ResourceError({error}:{error:unknown}){return error?<ErrorAlert error={ApiError.from(error)}/>:null;}
export function Empty({children}:{children:ReactNode}){return <p className={styles.empty}>{children}</p>;}
export function More({query}:{query:{hasNextPage:boolean;isFetchingNextPage:boolean;fetchNextPage:()=>unknown}}){return query.hasNextPage?<Button variant="secondary" disabled={query.isFetchingNextPage} onClick={()=>void query.fetchNextPage()}>Load more</Button>:null;}
export function Modal({title,close,children}:{title:string;close:()=>void;children:ReactNode}){return <Dialog open onOpenChange={open=>{if(!open)close();}}><DialogContent title={title}>{children}</DialogContent></Dialog>;}
export const todoStatuses=["yet_to_do","in_progress","blocked","completed","canceled"];
export const projectStatuses=["yet_to_start","in_progress","blocked","completed","canceled"];
export function StatusSelect({value,onChange,project=false,all=false,label:field="Status"}:{value:string;onChange:(value:string)=>void;project?:boolean;all?:boolean;label?:string}){return <Select label={field} value={value} onValueChange={onChange} options={[...(all?[{value:"all",label:"All statuses"}]:[]),...(project?projectStatuses.filter(value=>all||value!=="completed"):todoStatuses).map(value=>({value,label:label(value)}))]}/>;}
export function Status({value}:{value:string}){return <span className={styles.status} data-status={value} data-sq="surface">{label(value)}</span>;}
export function Who({actor}:{actor:Actor|string}){const a=typeof actor==="string"?{id:actor,type:actor.startsWith("si:")?"silicon" as const:"carbon" as const}:actor;return <AccountChip account={{...a,kind:a.type}} variant="chip"/>;}
export function Attachments({urls}:{urls:string[]}){return urls.length?<div className={styles.attachments}>{urls.map((url,i)=>{let href:string|undefined;try{const u=new URL(url);if(u.protocol==="https:")href=u.href;}catch{}return href?<a key={url} href={href} target="_blank" rel="noreferrer">Attachment {i+1} ↗</a>:null;})}</div>:null;}
