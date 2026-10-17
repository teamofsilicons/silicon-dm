/** Groups and invitations before DM 0.17: groups are conversations in the list now, and invitations open from it. */
import { redirect } from "next/navigation";

export default function OldGroups() {
  redirect("/c");
}
