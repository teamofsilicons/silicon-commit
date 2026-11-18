"use client";
import {useEffect, useId, useState} from "react";
import {useQuery} from "@tanstack/react-query";
import {api} from "@/lib/client/api";
import {AccountChip, type ChipAccount} from "@/components/foundation/account/account-chip";
import {Input} from "@/components/silicon-ui/input/input";
import {Button} from "@/components/silicon-ui/button/button";
import {Select} from "@/components/silicon-ui/select/select";
import {Page, PageHeader, Surface} from "@/components/foundation/layout/layout";
import {Feedback, ResourceError, mutate, useAction, useResource} from "./common";
import s from "./product.module.css";

export type Person=ChipAccount & {uuid:string;trust:number;sent:number;received:number};
type Target={id:string;type:string;name:string;org_id:string};
type Directory={organizations:{org_id:string;name:string}[];share_targets:Target[]};
export function useTeams(){
  const query=useResource<Directory>("/v1/organizations");
  return {...query,teams:(query.data?.share_targets??[]).filter(t=>t.type==="team").map(t=>({
    value:"team:"+t.id,label:t.name+" · "+(query.data?.organizations.find(o=>o.org_id===t.org_id)?.name??t.org_id)
  }))};
}
function useDelayed(value:string){
  const [delayed,setDelayed]=useState(value);
  useEffect(()=>{const timer=setTimeout(()=>setDelayed(value),180);return ()=>clearTimeout(timer);},[value]);
  return delayed.trim();
}
function usePerson(id:string){
  return useQuery({queryKey:["person",id.toLowerCase()],queryFn:({signal})=>api.get<Person>("/v1/people/resolve",{query:{id},signal}),enabled:/^(c:|si:|[0-9a-f]{8}-)/i.test(id)&&id.length>3,staleTime:60000,retry:false});
}
export function PersonChip({id,fallback,onRemove}:{id:string;fallback?:ChipAccount;onRemove?:()=>void}){
  const lookup=usePerson(id);
  return <AccountChip account={lookup.data??fallback??{id,kind:id.startsWith("si:")?"silicon":"carbon"}} variant="chip" onRemove={onRemove}/>;
}
type PersonFieldProps={label:string;value:string;onChange:(value:string)=>void;onPick?:(person:Person)=>void;description?:string;kind?:"carbon"|"silicon";exclude?:string[];required?:boolean};
export function PersonField({label,value,onChange,onPick,description,kind,exclude=[],required}:PersonFieldProps){
  const [open,setOpen]=useState(false),[active,setActive]=useState(-1);
  const id=useId(),query=useDelayed(value),exact=usePerson(query);
  const suggestions=useQuery({queryKey:["suggest",query],queryFn:({signal})=>api.get<{items:Person[]}>("/v1/people",{query:{q:query,limit:24},signal}),enabled:open,staleTime:30000});
  const items=[...(suggestions.data?.items??[])];
  if(exact.data&&!items.some(p=>p.uuid===exact.data.uuid)&&exact.data.trust>=0)items.push(exact.data);
  const choices=items.filter(p=>(!kind||p.kind===kind)&&!exclude.includes(p.id)).slice(0,10);
  const preview=exact.data&&query===value.trim()&&(!kind||kind===exact.data.kind)?exact.data:null;
  function select(person:Person){if(onPick)onPick(person);else onChange(person.id);setOpen(false);setActive(-1);}
  return <div className={s.field} onBlur={event=>{if(!event.currentTarget.contains(event.relatedTarget))setOpen(false);}}>
    <Input label={label} required={required} description={description} role="combobox" autoComplete="off" aria-expanded={open} aria-controls={id}
      aria-activedescendant={open&&choices[active]?id+"-"+active:undefined} aria-autocomplete="list" value={value}
      placeholder="Name, c: ID or si: ID" onFocus={()=>setOpen(true)} onChange={e=>{onChange(e.target.value);setActive(-1);setOpen(true);}}
      onKeyDown={e=>{
        if(e.key==="Escape"){e.preventDefault();setOpen(false);}
        if(e.key==="ArrowDown"||e.key==="ArrowUp"){e.preventDefault();setOpen(true);setActive(i=>Math.max(0,Math.min(choices.length-1,i+(e.key==="ArrowDown"?1:-1))));}
        if(e.key==="Enter"&&open){e.preventDefault();const chosen=choices[active]??preview??(choices.length===1?choices[0]:undefined);if(chosen)select(chosen);}
      }}/>
    {open?<div className={s.suggestions} id={id} role="listbox" aria-label={label+" suggestions"}>{choices.map((p,i)=><button type="button" role="option" id={id+"-"+i} key={p.uuid} aria-selected={i===active} onMouseDown={e=>e.preventDefault()} onClick={()=>select(p)}><AccountChip account={p}/></button>)}{!choices.length?<span className={s.muted}>{suggestions.isLoading?"Looking up people…":suggestions.error?"Suggestions are unavailable.":"No suggestions yet."}</span>:null}</div>:null}
    {preview&&!onPick?<AccountChip account={preview} variant="chip"/>:null}
  </div>;
}
export function Members({ids,onChange}:{ids:string[];onChange:(ids:string[])=>void}){
  const [draft,setDraft]=useState("");
  return <div className={s.field}><PersonField label="Members" value={draft} onChange={setDraft} exclude={ids} onPick={person=>{if(!ids.includes(person.id))onChange([...ids,person.id]);setDraft("");}}/>
    <div className={s.meta}>{ids.map(id=><PersonChip key={id} id={id} onRemove={()=>onChange(ids.filter(i=>i!==id))}/>)}</div></div>;
}
export function TeamSelect({value,onChange,label="Team"}:{value:string;onChange:(value:string)=>void;label?:string}){
  const teams=useTeams();const options=teams.teams;
  if(value&&!options.some(t=>t.value===value))options.push({value,label:value});
  return <><Select label={label} value={value||"none"} onValueChange={v=>onChange(v==="none"?"":v)} options={[{value:"none",label:"No team"},...options]}/><ResourceError error={teams.error}/></>;
}
export function TeamList({values,onChange}:{values:string[];onChange:(values:string[])=>void}){
  const {teams,error}=useTeams();return <fieldset className={s.checks}><legend>Invite teams</legend><ResourceError error={error}/>{[...new Set([...teams.map(t=>t.value),...values])].map(id=><label key={id}><input type="checkbox" checked={values.includes(id)} onChange={e=>onChange(e.target.checked?[...values,id]:values.filter(v=>v!==id))}/>{teams.find(t=>t.value===id)?.label??id}</label>)}{!teams.length&&!values.length?<span className={s.muted}>No teams are available for this account.</span>:null}</fieldset>;
}
type Contact={uuid:string;name:string;trust:number;photo_id?:string;account:Person};
export function Contacts(){
  const [offset,setOffset]=useState(0),[id,setId]=useState(""),[name,setName]=useState(""),[trust,setTrust]=useState("1");
  const data=useResource<{connected:boolean;items:Contact[];total:number}>("/v1/contacts?limit=50&offset="+offset);
  const action=useAction();
  return <Page width="reading"><PageHeader title="Contacts" description="The names, photos and trust levels in your Silicon Contacts address book."/><ResourceError error={data.error}/>
    {data.data?.connected?<><Surface><form className={s.form} onSubmit={e=>{e.preventDefault();void action.run(async()=>{await mutate("/v1/contacts","POST",{id,name,trust:Number(trust)});setId("");setName("");setTrust("1");},"Contact saved");}}>
      <PersonField label="Contact" value={id} onChange={setId} required onPick={p=>{setId(p.id);setName(p.display_name||p.id);setTrust(String(p.trust||1));}}/>
      <Input label="Saved name" required maxLength={200} value={name} onChange={e=>setName(e.target.value)}/>
      <Input label="Trust" type="number" min={1} max={255} required value={trust} onChange={e=>setTrust(e.target.value)}/>
      <Button type="submit" disabled={action.busy}>Save contact</Button></form></Surface>
      <Surface>{data.data.items.map(c=><div className={s.spread+" "+s.item} key={c.uuid}><AccountChip account={{...c.account,display_name:c.name,pfp_url:c.photo_id?"/api/v1/contacts/photos/"+c.photo_id:c.account.pfp_url}} note={"Trust "+c.trust}/><Button variant="secondary" onClick={()=>{setId(c.account.id);setName(c.name);setTrust(String(c.trust));}}>Edit</Button></div>)}
      {data.data.total===0?<p>No saved contacts yet.</p>:null}<div className={s.actions}>{offset>0?<Button onClick={()=>setOffset(offset-50)}>Previous contacts</Button>:null}{offset+50<data.data.total?<Button onClick={()=>setOffset(offset+50)}>More contacts</Button>:null}</div></Surface>
      <Button variant="ghost" onClick={()=>void action.run(()=>mutate("/v1/contacts/connection","PUT",{enabled:false}),"Contacts disconnected")}>Disconnect Contacts</Button></>:data.data?<Surface><p>Connect your address book to use saved names and photos, add contacts, and rank suggestions by trust.</p><Button onClick={()=>void action.run(()=>mutate("/v1/contacts/connection","PUT",{enabled:true}),"Contacts connected")}>Connect Silicon Contacts</Button></Surface>:null}
    <Feedback action={action}/></Page>;
}

