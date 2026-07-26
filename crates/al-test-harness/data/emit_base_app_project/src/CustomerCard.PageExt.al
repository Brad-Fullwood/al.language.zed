pageextension 50121 "Emitter Customer Card Ext" extends "Customer Card"
{
    layout
    {
        modify(Name)
        {
            Caption = 'Emitter Customer Name';
            ToolTip = 'Specifies the customer name for the emitter differential.';
        }
        addlast(General)
        {
            field("Emitter Note"; Rec."Emitter Note")
            {
                ApplicationArea = All;
                Caption = 'Emitter Note';
                ToolTip = 'Specifies the emitter differential note.';
            }
        }
    }
}
