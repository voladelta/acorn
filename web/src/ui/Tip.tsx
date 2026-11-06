/** @jsxImportSource octane */
// A hint shown as the pointer rests on what it explains: a shadcn
// tooltip (Base UI) whose trigger is the element given.

import type { OctaneNode } from 'octane';
import { Tooltip, TooltipContent, TooltipTrigger } from '@octanejs/shadcn/base-ui/Tooltip';

export function Tip(props: { tip: string; side?: 'top' | 'bottom' | 'left' | 'right'; children: OctaneNode }) {
  if (!props.tip) return props.children;
  return (
    <Tooltip>
      <TooltipTrigger render={props.children} />
      <TooltipContent side={props.side ?? 'top'} sideOffset={6} className="max-w-sm whitespace-pre-line">
        {props.tip}
      </TooltipContent>
    </Tooltip>
  );
}
