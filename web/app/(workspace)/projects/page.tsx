import {Projects} from "@/components/product/projects";
import type {Project,PageResult} from "@/components/product/types";
import {tryApiFetch} from "@/lib/server/rsc";
export default async function Page(){const result=await tryApiFetch<PageResult<Project>>("/v1/projects?limit=50");return <Projects initialData={result.data??undefined}/>;}
