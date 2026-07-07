report 50130 "Work Order Process Staging"
{
    UsageCategory = ReportsAndAnalysis;
    ApplicationArea = All;
    DefaultLayout = Word;

    dataset
    {
        dataitem(Staging; "Work Order Staging")
        {
            column(No; "No.") { }
            column(Description; Description) { }
            column(Status; Status) { }

            trigger OnAfterGetRecord()
            begin
                this.Helper.SchedulePost(Staging);
                this.PostTask.Run(Staging);
            end;
        }
    }

    var
        Helper: Codeunit "Work Order Helper";
        PostTask: Codeunit "Work Order Post Task";
        ActionType: Enum "Work Order Action";
        StagingRec: Record "Work Order Staging";

    procedure SetAction(NewAction: Enum "Work Order Action")
    begin
        ActionType := NewAction;
    end;

    procedure ProcessStagingRec()
    var
        Dummy: Text;
    begin
        Dummy := StagingRec.GetJournalData();
        if this.Helper.PrecheckRecord(StagingRec) then
            if StagingRec.Status = StagingRec.Status::Posting then
                this.PostTask.Run(StagingRec);
    end;
}
