import {Boards} from "@/components/product/boards";
export default async function Page({params}:{params:Promise<{id:string}>}){return <Boards id={(await params).id}/>;}
