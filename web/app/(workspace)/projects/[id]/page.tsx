import {ProjectDetail} from "@/components/product/projects";
import type {Project} from "@/components/product/types";
import {apiFetch} from "@/lib/server/rsc";
export default async function Page({params}:{params:Promise<{id:string}>}){const {id}=await params;const data=await apiFetch<Project>(`/v1/projects/${encodeURIComponent(id)}`,{notFound:true});return <ProjectDetail id={id} initialData={data}/>;}
