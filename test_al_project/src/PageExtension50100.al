pageextension 50100 "Test Customer Card Ext" extends "Test Customer Card"
{
    layout
    {
        addafter(Name)
        {
            field("Contact Name"; Rec."Contact Name")
            {
                ApplicationArea = All;
            }
        }
    }

    actions
    {
        addlast(Processing)
        {
            action(ShowContact)
            {
                Caption = 'Show Contact';
                ApplicationArea = All;

                trigger OnAction()
                begin
                    Message('Contact: %1', Rec."Contact Name");
                end;
            }
        }
    }
}
