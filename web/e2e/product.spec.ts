import {expect,test} from "@playwright/test";
import {accountsSession,runAccounts,signIn,sameOrigin,watchErrors,captureViews} from "./support";
test("two Carbons exchange messages, keep drafts, and join an invited group",async({browser})=>{
 const a=await accountsSession(browser,"carbon");const b=await accountsSession(browser,"friend");const page=await a.newPage();const friend=await b.newPage();const errors=watchErrors(page);
 await signIn(page,runAccounts().carbon.email);await signIn(friend,runAccounts().friend.email);
 await page.getByRole("button",{name:"New conversation",exact:true}).click();await page.getByLabel("Carbon or Silicon id").fill(runAccounts().friend.id);await page.getByRole("button",{name:"Open conversation",exact:true}).click();await expect(page).toHaveURL(/\/conversations\/.+/);
 await page.getByLabel("Message",{exact:true}).fill("A thought worth keeping");await page.getByRole("button",{name:"Send message",exact:true}).click();await expect(page.getByText("Message sent",{exact:true})).toBeVisible();await expect(page.getByText("A thought worth keeping",{exact:true})).toBeVisible();
 await captureViews(page,"conversation");const thread=page.url();await friend.goto(thread);await expect(friend.getByText("A thought worth keeping",{exact:true})).toBeVisible();
 await page.getByRole("button",{name:"Edit",exact:true}).click();await page.getByLabel("Edit message",{exact:true}).fill("A thought, made clearer");await page.getByRole("button",{name:"Save edit",exact:true}).click();await expect(page.getByText("Message updated",{exact:true})).toBeVisible();
 await page.getByLabel("Message",{exact:true}).fill("A draft for later");await page.getByRole("button",{name:"Save draft",exact:true}).click();await expect(page.getByText("Draft saved",{exact:true})).toBeVisible();await page.reload();await expect(page.getByLabel("Message",{exact:true})).toHaveValue("A draft for later");
 await page.goto("/groups");await page.getByRole("button",{name:"Create group",exact:true}).click();await page.getByLabel("Group name").fill("Working notes");await page.getByLabel("Invite accounts").fill(runAccounts().friend.id);await page.getByRole("button",{name:"Create group",exact:true}).last().click();await expect(page).toHaveURL(/\/groups\/.+/);
 await friend.goto("/groups");await expect(friend.getByRole("heading",{name:"Working notes",exact:true})).toBeVisible();await friend.getByRole("button",{name:"Accept invitation"}).click();await expect(friend.getByText("Invitation accepted",{exact:true})).toBeVisible();
 await page.reload();await page.getByLabel("Group name").fill("Working notes, updated");await page.getByRole("button",{name:"Save settings"}).click();await expect(page.getByText("Group settings saved",{exact:true})).toBeVisible();
 await captureViews(page,"group");await page.goto("/settings");await page.getByLabel("What happened?").fill("Browser verification report");await page.getByRole("button",{name:"Send report"}).click();await expect(page.getByText("Report sent",{exact:true})).toBeVisible();
 const rejected=await a.request.post("/api/api/v1/groups",{headers:{Origin:"https://elsewhere.example"},data:{type:"create_group",data:{name:"Forbidden"}}});expect(rejected.status()).toBe(403);
 const good=await a.request.get("/api/api/v1/me");expect(good.status()).toBe(200);expect((await good.json()).data.account.id).toBe(runAccounts().carbon.id);
 await a.request.post("/auth/sign-out",{headers:sameOrigin()});expect(errors).toEqual([]);await a.close();await b.close();
});
