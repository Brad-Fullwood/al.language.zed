page 50130 "Work Order Staging List"
{
    PageType = List;
    SourceTable = "Work Order Staging";
    Caption = 'Work Order Staging List';

    var
        ErrorMessageText: Text;
        RowStyle: Text;
        ProcessReport: Report "Work Order Process Staging";
        ActionType: Enum "Work Order Action";

    layout
    {
        area(Content)
        {
            repeater(Group)
            {
                field("No."; Rec."No.")
                {
                    ApplicationArea = All;
                }
                field(Description; Rec.Description)
                {
                    ApplicationArea = All;
                }
                field(Status; Rec.Status)
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
            action(Process)
            {
                Caption = 'Process';
                ApplicationArea = All;

                trigger OnAction()
                begin
                    this.ProcessReport.SetAction(this.ActionType::Precheck);
                end;
            }
        }
    }

    trigger OnAfterGetRecord()
    begin
        this.ErrorMessageText := CopyStr(Rec.Description, 1, MaxStrLen(this.ErrorMessageText));
        this.RowStyle := Format(Rec.SystemId);
        if Rec.Status = Rec.Status::Posted then
            this.RowStyle := 'Favorable';
        if Rec."Journal Data".HasValue then
            this.RowStyle := 'HasData';
    end;
}
