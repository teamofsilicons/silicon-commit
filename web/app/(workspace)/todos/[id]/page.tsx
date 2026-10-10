import {TodoDetail} from "@/components/product/todos";
import type {Todo} from "@/components/product/types";
import {apiFetch} from "@/lib/server/rsc";
export default async function Page({params}:{params:Promise<{id:string}>}){const {id}=await params;const data=await apiFetch<Todo>(`/v1/todos/${encodeURIComponent(id)}`,{notFound:true});return <TodoDetail id={id} initialData={data}/>;}
