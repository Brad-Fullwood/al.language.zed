page 50100 "Test Customer Card"
{
    PageType = Card;
    SourceTable = "Test Customer";
    Caption = 'Test Customer Card';

    layout
    {
        area(Content)
        {
            group(General)
            {
                field("No."; Rec."No.")
                {
                    ApplicationArea = All;
                }
                field(Name; Rec.Name)
                {
                    ApplicationArea = All;
                }
                field(Balance; Rec.Balance)
                {
                    ApplicationArea = All;
                    Editable = false;
                }
                field(Active; Rec.Active)
                {
                    ApplicationArea = All;
                }
            }
        }
    }

    actions
    {
        area(Processing)
        {
            action(CalcBalance)
            {
                Caption = 'Calculate Balance';
                ApplicationArea = All;

                trigger OnAction()
                begin
                    Message('Balance: %1', Rec.Balance);
                end;
            }
        }
    }
}
