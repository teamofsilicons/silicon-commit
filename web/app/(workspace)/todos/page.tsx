import {Todos} from "@/components/product/todos";
import type {Todo,PageResult} from "@/components/product/types";
import {tryApiFetch} from "@/lib/server/rsc";
export default async function Page(){const result=await tryApiFetch<PageResult<Todo>>("/v1/todos?view=assigned_to_me&limit=50");return <Todos initialData={result.data??undefined}/>;}
